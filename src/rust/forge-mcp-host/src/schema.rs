//! JSON argument schemas for the exposed Mission Control tools.

use serde_json::{json, Map, Value};

/// JSON Schema (as a serde_json object) for one tool's arguments.
pub fn input_schema(name: &str) -> Map<String, Value> {
    let schema = match name {
        "forge.work_get" => json!({
            "type": "object",
            "properties": { "task_id": { "type": "string" } },
            "required": ["task_id"]
        }),
        "forge.workflow_get" => json!({
            "type": "object",
            "properties": { "workflow_id": { "type": "string" } },
            "required": ["workflow_id"]
        }),
        "forge.events_list" => json!({
            "type": "object",
            "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 500 } }
        }),
        "forge.work_submit" => json!({
            "type": "object",
            "properties": {
                "project_alias": { "type": "string" },
                "base_revision": { "type": "string" },
                "agent_profile": { "type": "string" },
                "prompt": { "type": "string" },
                "acceptance_criteria": { "type": "string" },
                "idempotency_key": { "type": "string" },
                "workflow_policy_digest": { "type": "string" }
            },
            "required": ["project_alias", "base_revision", "agent_profile", "prompt",
                         "acceptance_criteria", "idempotency_key", "workflow_policy_digest"]
        }),
        "forge.work_cancel" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" },
                "expected_revision": { "type": "integer" }
            },
            "required": ["task_id", "expected_revision"]
        }),
        "forge.decide" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" },
                "expected_revision": { "type": "integer" },
                "approve": { "type": "boolean" },
                "idempotency_key": { "type": "string" }
            },
            "required": ["task_id", "expected_revision", "approve", "idempotency_key"]
        }),
        _ => json!({ "type": "object" }),
    };
    schema.as_object().cloned().unwrap_or_default()
}
