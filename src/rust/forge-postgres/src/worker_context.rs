use crate::error::AdapterError;
use crate::worker::WorkerApi;
use serde_json::Value;
use uuid::Uuid;

fn req_str(payload: &Value, field: &str) -> Result<String, AdapterError> {
    payload
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| AdapterError::Protocol(format!("{field} missing")))
}

fn opt_str(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn req_uuid(payload: &Value, field: &str) -> Result<Uuid, AdapterError> {
    let text = req_str(payload, field)?;
    Uuid::parse_str(&text).map_err(|_| AdapterError::Protocol(format!("{field} invalid")))
}

/// The native delivery context read through an attempt's authenticated
/// feedback: exact keys validated by the Python store
/// (read_native_delivery_context). delivery_failure stays a JSON
/// document - it is the recorded delivery failure payload.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeDeliveryContext {
    pub feedback_id: Uuid,
    pub feedback_sha256: String,
    pub review_id: Uuid,
    pub source_task_id: Uuid,
    pub source_attempt_id: Uuid,
    pub candidate_commit: String,
    pub gate_manifest_sha256: String,
    pub delivery_failure: Value,
}

impl NativeDeliveryContext {
    pub fn from_json(payload: Value) -> Result<NativeDeliveryContext, AdapterError> {
        Ok(NativeDeliveryContext {
            feedback_id: req_uuid(&payload, "feedback_id")?,
            feedback_sha256: req_str(&payload, "feedback_sha256")?,
            review_id: req_uuid(&payload, "review_id")?,
            source_task_id: req_uuid(&payload, "source_task_id")?,
            source_attempt_id: req_uuid(&payload, "source_attempt_id")?,
            candidate_commit: req_str(&payload, "candidate_commit")?,
            gate_manifest_sha256: req_str(&payload, "gate_manifest_sha256")?,
            delivery_failure: payload
                .get("delivery_failure")
                .cloned()
                .ok_or_else(|| AdapterError::Protocol("delivery_failure missing".to_string()))?,
        })
    }
}

impl WorkerApi {
    /// Reads the reviewed plan document for this attempt
    /// (forge_coding_worker_api.read_plan). The document stays JSON:
    /// its schema belongs to the plan/review contexts (S3).
    pub fn read_plan(
        &mut self,
        claim: &crate::worker::Claim,
    ) -> Result<Option<Value>, AdapterError> {
        self.call_json_for_claim("SELECT forge_coding_worker_api.read_plan($1, $2)", claim)
    }

    /// Reads the feedback document for this attempt
    /// (forge_coding_worker_api.read_feedback). JSON for the same
    /// reason as read_plan.
    pub fn read_feedback(
        &mut self,
        claim: &crate::worker::Claim,
    ) -> Result<Option<Value>, AdapterError> {
        self.call_json_for_claim(
            "SELECT forge_coding_worker_api.read_feedback($1, $2)",
            claim,
        )
    }

    /// Reads the workflow feedback document for this attempt
    /// (forge_coding_worker_api.read_workflow_feedback).
    pub fn read_workflow_feedback(
        &mut self,
        claim: &crate::worker::Claim,
    ) -> Result<Option<Value>, AdapterError> {
        self.call_json_for_claim(
            "SELECT forge_coding_worker_api.read_workflow_feedback($1, $2)",
            claim,
        )
    }

    /// Reads the review target document for this attempt
    /// (forge_coding_worker_api.read_review_target).
    pub fn read_review_target(
        &mut self,
        claim: &crate::worker::Claim,
    ) -> Result<Option<Value>, AdapterError> {
        self.call_json_for_claim(
            "SELECT forge_coding_worker_api.read_review_target($1, $2)",
            claim,
        )
    }

    /// Reads the external review context through the external
    /// workflow schema (forge_external_workflow_worker_api.read_context).
    pub fn read_external_review_context(
        &mut self,
        claim: &crate::worker::Claim,
    ) -> Result<Option<Value>, AdapterError> {
        self.call_json_for_claim(
            "SELECT forge_external_workflow_worker_api.read_context($1, $2)",
            claim,
        )
    }

    /// Reads the typed native delivery context for this attempt.
    pub fn read_native_delivery_context(
        &mut self,
        claim: &crate::worker::Claim,
    ) -> Result<Option<NativeDeliveryContext>, AdapterError> {
        let payload = self.call_json_for_claim(
            "SELECT forge_coding_worker_api.read_native_delivery_context($1, $2)",
            claim,
        )?;
        payload.map(NativeDeliveryContext::from_json).transpose()
    }

    /// The durable merged heads for this owner and the worker's
    /// project grants: (project_alias, head sha1) pairs
    /// (forge_coding_worker_api.project_revisions).
    pub fn project_revisions(&mut self) -> Result<Vec<(String, String)>, AdapterError> {
        let payload =
            self.call_json_no_args("SELECT forge_coding_worker_api.project_revisions()")?;
        let Some(Value::Object(map)) = payload else {
            return Ok(Vec::new());
        };
        let mut pairs = Vec::with_capacity(map.len());
        for (alias, head) in map {
            let Some(head) = head.as_str() else {
                return Err(AdapterError::Protocol(
                    "project_revisions value is not a string".to_string(),
                ));
            };
            pairs.push((alias, head.to_string()));
        }
        Ok(pairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_delivery_context_decodes() {
        let ctx = NativeDeliveryContext::from_json(json!({
            "feedback_id": "11111111-1111-1111-1111-111111111111",
            "feedback_sha256": "ab12",
            "review_id": "22222222-2222-2222-2222-222222222222",
            "source_task_id": "33333333-3333-3333-3333-333333333333",
            "source_attempt_id": "44444444-4444-4444-4444-444444444444",
            "candidate_commit": "0123456789abcdef0123456789abcdef01234567",
            "gate_manifest_sha256": "cd34",
            "delivery_failure": {"reason": "gate"}
        }))
        .unwrap();
        assert_eq!(ctx.candidate_commit.len(), 40);
        assert_eq!(ctx.delivery_failure["reason"], "gate");
    }

    #[test]
    fn native_delivery_context_missing_key_is_protocol() {
        let mut bad = json!({
            "feedback_id": "11111111-1111-1111-1111-111111111111",
            "feedback_sha256": "ab12",
            "review_id": "22222222-2222-2222-2222-222222222222",
            "source_task_id": "33333333-3333-3333-3333-333333333333",
            "source_attempt_id": "44444444-4444-4444-4444-444444444444",
            "candidate_commit": "0123456789abcdef0123456789abcdef01234567",
            "gate_manifest_sha256": "cd34"
        });
        assert!(matches!(
            NativeDeliveryContext::from_json(bad.clone()),
            Err(AdapterError::Protocol(_))
        ));
        bad["delivery_failure"] = json!({});
        assert!(NativeDeliveryContext::from_json(bad).is_ok());
    }
}
