use serde_json::Value;
use uuid::Uuid;

/// The bounded task projection every read and control outcome carries:
/// the Python _summary dict, field for field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSummary {
    /// The task's id.
    pub task_id: Uuid,
    /// The correlation id shared by the task's whole workflow.
    pub correlation_id: Uuid,
    /// The project the task belongs to.
    pub project_alias: String,
    /// The main revision the task was admitted against.
    pub base_revision: String,
    /// The task mode (plan, implement, review, ...).
    pub mode: String,
    /// The agent profile the task is pinned to.
    pub agent_profile: String,
    /// The digest of the recipe pinned at admission.
    pub recipe_digest: String,
    /// The task's current status string.
    pub status: String,
    /// The optimistic-concurrency counter; control operations carry it
    /// as expected_revision and fail if it has moved.
    pub revision: i64,
    /// Creation time, as an ISO timestamp string.
    pub created_at: String,
    /// The task's display title, when one has been recorded.
    pub title: Option<String>,
    /// The first-class task kind (main delta 8b921df): the
    /// row's task_kind, or "feature" when the projection does
    /// not carry the column.
    pub task_kind: String,
    /// The defect document for a defect task, when the
    /// projection carries it.
    pub defect: Option<Value>,
    /// Defects filed against this task or its workflow
    /// ({task_id, title, severity} rows, newest query order,
    /// capped at 50 by the store query).
    pub filed_defects: Vec<Value>,
    /// Whether the task belongs to a workflow, when the projection
    /// carries it.
    pub workflow_required: Option<bool>,
    /// The workflow's current phase, when the projection carries it.
    pub workflow_phase: Option<String>,
    /// Scope activity time; absent for legacy raw-status pages.
    pub updated_at: Option<String>,
    /// Current workflow automation switch, when available.
    pub workflow_enabled: Option<bool>,
    /// Protected delivery projection, when configured and requested.
    pub delivery: Option<Value>,
    /// Retained retry budget projection.
    pub retry_event: Option<Value>,
}

/// One task in full: its summary plus the public detail projection.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskView {
    /// The bounded summary projection.
    pub summary: TaskSummary,
    /// The public progress/workflow projection. Carried as a JSON value
    /// in v1 - the recorded bring-up debt of this crate family (as in
    /// forge-postgres's Claim::raw): typed projection structs are a
    /// later increment.
    pub projection: Value,
}

/// One coding_events row as a typed view.
#[derive(Debug, Clone, PartialEq)]
pub struct EventView {
    pub event_id: Uuid,
    pub correlation_id: Uuid,
    /// Guarded public fields; raw payload stays internal.
    pub public: Value,
    /// The event's sequence within the task's stream.
    pub sequence: i64,
    /// The event kind, from the canonical vocabulary.
    pub kind: crate::work_events::EventKind,
    /// Who acted, when the event payload carries a validated actor.
    pub actor: Option<crate::work_actors::Actor>,
    /// The cause string, when the event kind requires one.
    pub cause: Option<String>,
    /// The event payload as stored.
    pub payload: Value,
    /// Emission time, as an ISO timestamp string.
    pub created_at: String,
}

/// One bounded chunk of an attempt's result content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultChunk {
    /// The digest of the full result the chunk belongs to.
    pub result_digest: String,
    /// The chunk's content.
    pub content: String,
    /// The byte offset this chunk starts at.
    pub offset_bytes: i64,
    /// The cursor for the next chunk, when more content remains.
    pub next_cursor: Option<String>,
}

/// One bounded view of a CI/CD stage log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageLogView {
    pub project_alias: String,
    pub task_id: Uuid,
    pub correlation_id: Uuid,
    pub action_id: Uuid,
    pub stage_id: String,
    pub view: String,
    pub content: String,
    pub truncated: bool,
    pub total_bytes: usize,
    pub byte_offset: usize,
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub tests_passed: Option<i64>,
    pub tests_failed: Option<i64>,
    pub tests_skipped: Option<i64>,
    pub test_cases: Vec<Value>,
    pub error_excerpt: Option<String>,
}

/// Complete scope counts before filtering or pagination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeCounts {
    pub active: u64,
    pub attention: u64,
    pub history: u64,
    pub all: u64,
}
impl ScopeCounts {
    pub fn from_value(value: &Value) -> Option<Self> {
        if value.as_object()?.len() != 4 {
            return None;
        }
        let result = Self {
            active: value["active"].as_u64()?,
            attention: value["attention"].as_u64()?,
            history: value["history"].as_u64()?,
            all: value["all"].as_u64()?,
        };
        result.is_valid().then_some(result)
    }
    pub fn to_value(&self) -> Value {
        serde_json::json!({"active":self.active,"attention":self.attention,"history":self.history,"all":self.all})
    }
    pub fn is_valid(&self) -> bool {
        self.all <= crate::operator_dtos::MAX_REVISION as u64
            && self
                .active
                .checked_add(self.attention)
                .and_then(|n| n.checked_add(self.history))
                == Some(self.all)
    }
}

/// A cursor page of task summaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPage {
    /// The page's summaries, in cursor order.
    pub items: Vec<TaskSummary>,
    /// The task id to continue from, when more tasks remain.
    pub next_cursor: Option<Uuid>,
    /// Counts before scope selection and keyset pagination.
    pub scope_counts: Option<ScopeCounts>,
}

/// A sequence page of events.
#[derive(Debug, Clone, PartialEq)]
pub struct EventPage {
    /// The page's events, in sequence order.
    pub items: Vec<EventView>,
    /// The sequence to continue after.
    pub next_after: i64,
}

/// The outcome of an idempotent control operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlOutcome {
    /// The control receipt that records the operation.
    pub receipt_id: Uuid,
    /// The task as it stands after the operation.
    pub task: TaskSummary,
    /// True when this outcome replays an earlier identical intent
    /// instead of acting again.
    pub replayed: bool,
    /// Immutable private receipt, projected per operation at the API boundary.
    /// This is never serialized wholesale; actor/audit fields remain private.
    pub recorded_receipt: Option<Value>,
}

impl ControlOutcome {
    pub fn with_recorded_receipt(mut self, receipt: Value) -> Self {
        self.recorded_receipt = Some(receipt);
        self
    }
}

/// A pinned policy identity: the kind of policy and its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIdentity {
    /// The policy kind (recipe, workflow, ...).
    pub kind: String,
    /// The snapshot's pinned digest.
    pub digest: String,
}

#[cfg(test)]
mod scope_count_tests {
    use super::*;
    #[test]
    fn counts_reject_negative_overflow_unknown_and_inconsistent_totals() {
        for value in [
            serde_json::json!({"active":-1,"attention":0,"history":0,"all":0}),
            serde_json::json!({"active":u64::MAX,"attention":1,"history":0,"all":0}),
            serde_json::json!({"active":1,"attention":0,"history":0,"all":0}),
            serde_json::json!({"active":0,"attention":0,"history":0,"all":0,"extra":1}),
        ] {
            assert!(ScopeCounts::from_value(&value).is_none());
        }
        let value = serde_json::json!({"active":1,"attention":2,"history":3,"all":6});
        assert_eq!(ScopeCounts::from_value(&value).unwrap().to_value(), value);
    }
}
