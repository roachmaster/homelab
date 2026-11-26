use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Requests the MCP layer can make of Mission Control. One variant per
/// operation; the payload sum is finite and application-owned, so a new
/// operation is a new variant plus a registry entry — never a
/// stringly-typed call or a generic JSON escape hatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McRequest {
    WorkGet {
        task_id: String,
    },
    WorkflowGet {
        workflow_id: String,
    },
    EventsList {
        limit: u32,
    },
    WorkSubmit(Box<WorkSubmit>),
    WorkCancel {
        task_id: String,
        expected_revision: u64,
    },
    Decide(Box<Decide>),
}

/// Submission payload; mirrors the dispatch `work_submit` DTO field for
/// field so the adapter is a translation, not a redesign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkSubmit {
    pub project_alias: String,
    pub base_revision: String,
    pub agent_profile: String,
    pub prompt: String,
    pub acceptance_criteria: String,
    pub idempotency_key: String,
    pub workflow_policy_digest: String,
}

/// Decision payload for plan/review decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decide {
    pub task_id: String,
    pub expected_revision: u64,
    pub approve: bool,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McResponse {
    Work(WorkView),
    Workflow(WorkflowView),
    Events(Vec<EventView>),
    Submitted { task_id: String },
    Cancelled { task_id: String },
    Decided { task_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkView {
    pub task_id: String,
    pub status: String,
    pub agent_profile: String,
    pub base_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowView {
    pub workflow_id: String,
    pub phase: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventView {
    pub sequence: u64,
    pub kind: String,
    pub task_id: Option<String>,
}

/// Why a request failed. Dispatch rejections stay distinguishable
/// (stale base vs stale digest vs revoked grant) — see the
/// dispatch-digest-diagnosis defect in the resubmission queue; an MCP
/// error that collapses them would repeat the defect at a new layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McError {
    StaleBase { expected: String, got: String },
    StaleDigest,
    GrantRevoked { profile: String },
    NotFound { what: String },
    InvalidArgs { detail: String },
    Unavailable { detail: String },
}

impl McError {
    /// Stable machine-readable code for MCP error data.
    pub fn code(&self) -> &'static str {
        match self {
            McError::StaleBase { .. } => "stale_base",
            McError::StaleDigest => "stale_digest",
            McError::GrantRevoked { .. } => "grant_revoked",
            McError::NotFound { .. } => "not_found",
            McError::InvalidArgs { .. } => "invalid_args",
            McError::Unavailable { .. } => "unavailable",
        }
    }
    pub fn message(&self) -> String {
        match self {
            McError::StaleBase { expected, got } => {
                format!("stale base: expected {expected}, got {got}")
            }
            McError::StaleDigest => "stale workflow policy digest".into(),
            McError::GrantRevoked { profile } => {
                format!("grant revoked or missing for profile {profile}")
            }
            McError::NotFound { what } => format!("not found: {what}"),
            McError::InvalidArgs { detail } => format!("invalid arguments: {detail}"),
            McError::Unavailable { detail } => format!("unavailable: {detail}"),
        }
    }
}

impl std::fmt::Display for McError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message(), self.code())
    }
}
impl std::error::Error for McError {}

/// The consumer-owned port. Implementations: dispatch HTTP (writes),
/// read-only Postgres (reads), in-memory fake (tests).
pub trait MissionControlPort: Send + Sync {
    fn execute(
        &self,
        request: McRequest,
    ) -> impl std::future::Future<Output = Result<McResponse, McError>> + Send;
}

/// Static description of one MCP tool. The registry is data, so the
/// protocol layer, the help surface, and the contract tests all derive
/// from the same descriptors — no drift between docs and behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub read_only: bool,
    pub destructive: bool,
    pub summary: &'static str,
}

pub const TOOLS: &[ToolDescriptor] = &[
    ToolDescriptor {
        name: "forge.work_get",
        read_only: true,
        destructive: false,
        summary: "Read one task's status, profile, and base revision.",
    },
    ToolDescriptor {
        name: "forge.workflow_get",
        read_only: true,
        destructive: false,
        summary: "Read one workflow's phase and enabled state.",
    },
    ToolDescriptor {
        name: "forge.events_list",
        read_only: true,
        destructive: false,
        summary: "List recent Forge lifecycle events, newest first.",
    },
    ToolDescriptor {
        name: "forge.work_submit",
        read_only: false,
        destructive: false,
        summary: "Submit new work through the runner dispatch API.",
    },
    ToolDescriptor {
        name: "forge.work_cancel",
        read_only: false,
        destructive: true,
        summary: "Cancel a workflow by its root task.",
    },
    ToolDescriptor {
        name: "forge.decide",
        read_only: false,
        destructive: true,
        summary: "Approve or reject a plan or review.",
    },
];

pub fn tool_descriptor(name: &str) -> Option<&'static ToolDescriptor> {
    TOOLS.iter().find(|t| t.name == name)
}

fn req_str(args: &Map<String, Value>, key: &str) -> Result<String, McError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| McError::InvalidArgs {
            detail: format!("missing string field '{key}'"),
        })
}

fn req_u64(args: &Map<String, Value>, key: &str) -> Result<u64, McError> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| McError::InvalidArgs {
            detail: format!("missing integer field '{key}'"),
        })
}

/// Translate one MCP tool call (name + JSON arguments) into a port
/// request. Unknown tools and malformed arguments are `InvalidArgs`,
/// rejected here — before any port effect (rejection before effect).
pub fn parse_tool_call(name: &str, args: &Value) -> Result<McRequest, McError> {
    let obj = args.as_object().cloned().unwrap_or_default();
    match name {
        "forge.work_get" => Ok(McRequest::WorkGet {
            task_id: req_str(&obj, "task_id")?,
        }),
        "forge.workflow_get" => Ok(McRequest::WorkflowGet {
            workflow_id: req_str(&obj, "workflow_id")?,
        }),
        "forge.events_list" => {
            let limit = obj.get("limit").and_then(Value::as_u64).unwrap_or(50) as u32;
            Ok(McRequest::EventsList {
                limit: limit.clamp(1, 500),
            })
        }
        "forge.work_submit" => {
            let submit: WorkSubmit =
                serde_json::from_value(args.clone()).map_err(|e| McError::InvalidArgs {
                    detail: format!("work_submit payload: {e}"),
                })?;
            Ok(McRequest::WorkSubmit(Box::new(submit)))
        }
        "forge.work_cancel" => Ok(McRequest::WorkCancel {
            task_id: req_str(&obj, "task_id")?,
            expected_revision: req_u64(&obj, "expected_revision")?,
        }),
        "forge.decide" => {
            let decide: Decide =
                serde_json::from_value(args.clone()).map_err(|e| McError::InvalidArgs {
                    detail: format!("decide payload: {e}"),
                })?;
            Ok(McRequest::Decide(Box::new(decide)))
        }
        _ => Err(McError::InvalidArgs {
            detail: format!("unknown tool '{name}'"),
        }),
    }
}

/// Serialize a port response as the tool result's structured content.
pub fn response_to_json(response: &McResponse) -> Value {
    match response {
        McResponse::Work(w) => json!({ "work": w }),
        McResponse::Workflow(w) => json!({ "workflow": w }),
        McResponse::Events(events) => json!({ "events": events }),
        McResponse::Submitted { task_id } => json!({ "submitted": { "task_id": task_id } }),
        McResponse::Cancelled { task_id } => json!({ "cancelled": { "task_id": task_id } }),
        McResponse::Decided { task_id } => json!({ "decided": { "task_id": task_id } }),
    }
}

/// The task-authoring prompt template: assignment and acceptance
/// criteria as two separate blocks (Forge house format).
pub const TASK_AUTHORING_PROMPT: &str = "\
Author a Forge task. Return exactly two separate blocks, in order:\n\
\n\
1. Assignment — one code block containing only the task description: \
the problem, the evidence for it, and what to build or change.\n\
2. Acceptance criteria — one code block containing only the verifiable \
conditions the finished work must meet.\n\
\n\
Never blend the two into a single block. Write the prompt for the full \
plan-to-implement flow unless the user asked for design only.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_entries_all_parse() {
        // Drift check: every registered tool must be parseable.
        for tool in TOOLS {
            let args = match tool.name {
                "forge.work_get" => json!({ "task_id": "t1" }),
                "forge.workflow_get" => json!({ "workflow_id": "w1" }),
                "forge.events_list" => json!({}),
                "forge.work_submit" => json!({
                    "project_alias": "homelab", "base_revision": "abc",
                    "agent_profile": "agy-primary", "prompt": "p",
                    "acceptance_criteria": "ac", "idempotency_key": "k",
                    "workflow_policy_digest": "d"
                }),
                "forge.work_cancel" => json!({ "task_id": "t1", "expected_revision": 3 }),
                "forge.decide" => json!({
                    "task_id": "t1", "expected_revision": 3,
                    "approve": true, "idempotency_key": "k"
                }),
                other => panic!("registry tool {other} has no test payload"),
            };
            assert!(parse_tool_call(tool.name, &args).is_ok(), "{}", tool.name);
        }
    }

    #[test]
    fn unknown_tool_rejected_before_effect() {
        let err = parse_tool_call("forge.nope", &json!({})).unwrap_err();
        assert_eq!(err.code(), "invalid_args");
    }

    #[test]
    fn missing_field_is_invalid_args() {
        let err = parse_tool_call("forge.work_get", &json!({})).unwrap_err();
        assert_eq!(err.code(), "invalid_args");
    }

    #[test]
    fn error_codes_are_distinct() {
        let codes = [
            McError::StaleBase {
                expected: "a".into(),
                got: "b".into(),
            }
            .code(),
            McError::StaleDigest.code(),
            McError::GrantRevoked {
                profile: "p".into(),
            }
            .code(),
            McError::NotFound { what: "w".into() }.code(),
        ];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len());
    }

    #[test]
    fn events_limit_clamped() {
        match parse_tool_call("forge.events_list", &json!({ "limit": 99999 })).unwrap() {
            McRequest::EventsList { limit } => assert_eq!(limit, 500),
            _ => panic!("wrong variant"),
        }
    }
}
