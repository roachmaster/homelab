use uuid::Uuid;

use crate::work_common;
use crate::work_read_cicd;
use crate::work_read_events;
use crate::work_read_get;
use crate::work_read_list;
use crate::work_read_result;
use crate::work_store::PgWorkStore;

/// The WorkReadPort impl: every method checks the project against
/// the store's allowlist first (require_project), then delegates to
/// its work_read_* target module.
impl forge_work::WorkReadPort for PgWorkStore {
    fn get(
        &self,
        project_alias: &str,
        task_id: Uuid,
    ) -> Result<forge_work::TaskView, forge_work::WorkError> {
        work_common::require_project(self, project_alias)?;
        work_read_get::get(self, project_alias, task_id)
    }

    fn list_tasks(
        &self,
        project_alias: &str,
        status_filter: Option<&str>,
        limit: i64,
        cursor_task_id: Option<Uuid>,
        delivery_visible: bool,
        task_kind_filter: Option<&str>,
    ) -> Result<forge_work::work_types::TaskPage, forge_work::WorkError> {
        work_common::require_project(self, project_alias)?;
        work_read_list::list_tasks(
            self,
            project_alias,
            status_filter,
            limit,
            cursor_task_id,
            delivery_visible,
            task_kind_filter,
        )
    }

    fn events(
        &self,
        project_alias: &str,
        task_id: Uuid,
        after_sequence: i64,
        limit: i64,
    ) -> Result<forge_work::work_types::EventPage, forge_work::WorkError> {
        work_common::require_project(self, project_alias)?;
        work_read_events::events(self, project_alias, task_id, after_sequence, limit)
    }

    fn result(
        &self,
        project_alias: &str,
        task_id: Uuid,
        attempt_id: Uuid,
        cursor: Option<&str>,
        limit_bytes: i64,
    ) -> Result<forge_work::work_types::ResultChunk, forge_work::WorkError> {
        work_common::require_project(self, project_alias)?;
        work_read_result::result(
            self,
            project_alias,
            task_id,
            attempt_id,
            cursor,
            limit_bytes,
        )
    }

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
    ) -> Result<forge_work::work_types::StageLogView, forge_work::WorkError> {
        work_common::require_project(self, project_alias)?;
        work_read_cicd::cicd_stage_log(
            self,
            project_alias,
            task_id,
            correlation_id,
            action_id,
            crate::work_read_cicd::LogPage {
                stage_id,
                view,
                cursor_token,
                limit_bytes: 32768,
            },
            delivery_enabled,
        )
    }
    fn cicd_stage_log_request(
        &self,
        request: forge_work::work_requests::StageLogReadRequest<'_>,
    ) -> Result<forge_work::work_types::StageLogView, forge_work::WorkError> {
        work_common::require_project(self, request.project_alias)?;
        work_read_cicd::cicd_stage_log(
            self,
            request.project_alias,
            request.task_id,
            request.correlation_id,
            request.action_id,
            crate::work_read_cicd::LogPage {
                stage_id: request.stage_id,
                view: request.view,
                cursor_token: request.cursor_token,
                limit_bytes: request.limit_bytes,
            },
            request.delivery_enabled,
        )
    }
}
