//! In-memory fake backend: canned Mission Control state for demos and
//! tests. This is the default backend — selecting it has zero external
//! effects, per the Forge wiring rule that an unused default must do
//! nothing.

use forge_mcp::{
    EventView, McError, McRequest, McResponse, MissionControlPort, WorkflowView, WorkView,
};

pub struct FakePort;

impl MissionControlPort for FakePort {
    async fn execute(&self, request: McRequest) -> Result<McResponse, McError> {
        match request {
            McRequest::WorkGet { task_id } => Ok(McResponse::Work(WorkView {
                task_id,
                status: "awaiting_review".into(),
                agent_profile: "muse-db-primary".into(),
                base_revision: "0070e78f9b567ccffcc7a7fdd8c9caeaf44275ff".into(),
            })),
            McRequest::WorkflowGet { workflow_id } => Ok(McResponse::Workflow(WorkflowView {
                workflow_id,
                phase: "implement".into(),
                enabled: true,
            })),
            McRequest::EventsList { limit } => {
                let kinds = ["submitted", "review_decided", "started", "queued", "progress"];
                let events = (0..limit.min(5))
                    .map(|i| EventView {
                        sequence: 2000 - i as u64,
                        kind: kinds[i as usize % kinds.len()].into(),
                        task_id: Some("6fdff749-0000-4000-8000-000000000000".into()),
                    })
                    .collect();
                Ok(McResponse::Events(events))
            }
            McRequest::WorkSubmit(_) => Ok(McResponse::Submitted {
                task_id: uuid::Uuid::new_v4().to_string(),
            }),
            McRequest::WorkCancel { task_id, .. } => Ok(McResponse::Cancelled { task_id }),
            McRequest::Decide(d) => Ok(McResponse::Decided { task_id: d.task_id }),
        }
    }
}
