use crate::work_error::WorkError;
use crate::work_types::{EventPage, ResultChunk, StageLogView, TaskPage, TaskView};
use uuid::Uuid;

/// The read side of the Python WorkStore - get, list_tasks, events, result, cicd_stage_log - as one synchronous trait in pure domain types.
pub trait WorkReadPort {
    /// get - Retrieve a task view by project alias and task ID.
    fn get(&self, project_alias: &str, task_id: Uuid) -> Result<TaskView, WorkError>;

    /// list_tasks - List tasks by project alias, status filter, limit, cursor task ID, and delivery visibility.
    fn list_tasks(
        &self,
        project_alias: &str,
        status_filter: Option<&str>,
        limit: i64,
        cursor_task_id: Option<Uuid>,
        delivery_visible: bool,
        task_kind_filter: Option<&str>,
    ) -> Result<TaskPage, WorkError>;

    /// events - Retrieve events for a task by project alias, task ID, after sequence, and limit.
    fn events(
        &self,
        project_alias: &str,
        task_id: Uuid,
        after_sequence: i64,
        limit: i64,
    ) -> Result<EventPage, WorkError>;

    /// result - Retrieve a result chunk for a task by project alias, task ID, attempt ID, cursor, and limit bytes.
    fn result(
        &self,
        project_alias: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        cursor: Option<&str>,
        limit_bytes: i64,
    ) -> Result<ResultChunk, WorkError>;

    /// cicd_stage_log - Retrieve a stage log view for a task by project alias, task ID, correlation ID, action ID, stage ID, view, and cursor token.
    fn cicd_stage_log(
        &self,
        project_alias: &str,
        task_id: Uuid,
        correlation_id: Uuid,
        action_id: Uuid,
        stage_id: &str,
        view: &str,
        cursor_token: Option<&str>,
        delivery_enabled: bool,
    ) -> Result<StageLogView, WorkError>;
}
