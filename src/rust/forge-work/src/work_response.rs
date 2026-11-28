use crate::catalog::WorkCatalog;
use crate::port_admission::WorkAdmissionPort;
use crate::port_decision::WorkDecisionPort;
use crate::port_read::WorkReadPort;
use crate::work_types::{ControlOutcome, EventPage, ResultChunk, StageLogView, TaskPage, TaskView};

/// The response sum every handler returns - one variant per port return shape.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkResponse {
    Control(ControlOutcome),
    Task(TaskView),
    Tasks(TaskPage),
    Events(EventPage),
    Result(ResultChunk),
    StageLog(StageLogView),
}

/// The per-dispatch context the host builds - the three ports, the frozen catalog, and the admission_open readiness flag.
pub struct WorkContext<'a> {
    pub read: &'a dyn WorkReadPort,
    pub admission: &'a dyn WorkAdmissionPort,
    pub decision: &'a dyn WorkDecisionPort,
    pub catalog: &'a WorkCatalog,
    pub admission_open: bool,
}

impl<'a> WorkContext<'a> {
    /// Creates a new `WorkContext` with the given ports, catalog, and readiness flag.
    pub fn new(
        read: &'a dyn WorkReadPort,
        admission: &'a dyn WorkAdmissionPort,
        decision: &'a dyn WorkDecisionPort,
        catalog: &'a WorkCatalog,
        admission_open: bool,
    ) -> WorkContext<'a> {
        WorkContext {
            read,
            admission,
            decision,
            catalog,
            admission_open,
        }
    }
}
