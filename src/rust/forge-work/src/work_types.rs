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
    /// Whether the task belongs to a workflow, when the projection
    /// carries it.
    pub workflow_required: Option<bool>,
    /// The workflow's current phase, when the projection carries it.
    pub workflow_phase: Option<String>,
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
    /// The stage the log belongs to.
    pub stage_id: String,
    /// The log content in this view.
    pub content: String,
    /// The cursor for the next view, when more log remains.
    pub next_cursor: Option<String>,
}

/// A cursor page of task summaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPage {
    /// The page's summaries, in cursor order.
    pub items: Vec<TaskSummary>,
    /// The task id to continue from, when more tasks remain.
    pub next_cursor: Option<Uuid>,
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
}

/// A pinned policy identity: the kind of policy and its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIdentity {
    /// The policy kind (recipe, workflow, ...).
    pub kind: String,
    /// The snapshot's pinned digest.
    pub digest: String,
}
