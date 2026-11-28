//! Step bindings for features/forge_work_bridge.feature, driving the real
//! fifteen-route WorkBridge and WorkAdmissionPolicy in forge-work.

use forge_core::bridge::DispatchError;
use forge_core::ingress::{Envelope, Header, Response, RouteKey};
use forge_scene::bdd::{ensure, verify, Steps};
use forge_work::catalog::WorkCatalog;
use forge_work::decision_requests::{
    ExternalOverrideRequest, ExternalRecoveryRequest, HandoffAuthority, HandoffRequest,
    PlanDecideRequest, PublicationRetryRequest, RepairRequest, ReviewDecideRequest,
};
use forge_work::port_admission::WorkAdmissionPort;
use forge_work::port_decision::WorkDecisionPort;
use forge_work::port_read::WorkReadPort;
use forge_work::work_error::WorkError;
use forge_work::work_payload::{route_key, WorkKind, WorkPayload};
use forge_work::work_requests::{AcceptRequest, CancelRequest, ResumeRequest, SubmitRequest};
use forge_work::work_response::{WorkContext, WorkResponse};
use forge_work::work_service::work_bridge;
use forge_work::work_types::{
    ControlOutcome, EventPage, PolicyIdentity, ResultChunk, StageLogView, TaskPage, TaskView,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

struct CountingPorts {
    calls: AtomicUsize,
}

impl CountingPorts {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl WorkReadPort for CountingPorts {
    fn get(&self, _project_alias: &str, _task_id: Uuid) -> Result<TaskView, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("read_port_boom"))
    }

    fn list_tasks(
        &self,
        _project_alias: &str,
        _status_filter: Option<&str>,
        _limit: i64,
        _cursor_task_id: Option<Uuid>,
        _delivery_visible: bool,
        _task_kind_filter: Option<&str>,
    ) -> Result<TaskPage, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(TaskPage {
            items: Vec::new(),
            next_cursor: None,
            scope_counts: None,
        })
    }

    fn events(
        &self,
        _project_alias: &str,
        _task_id: Uuid,
        _after_sequence: i64,
        _limit: i64,
    ) -> Result<EventPage, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn result(
        &self,
        _project_alias: &str,
        _task_id: Uuid,
        _attempt_id: Uuid,
        _cursor: Option<&str>,
        _limit_bytes: i64,
    ) -> Result<ResultChunk, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn cicd_stage_log(
        &self,
        _project_alias: &str,
        _task_id: Uuid,
        _correlation_id: Uuid,
        _action_id: Uuid,
        _stage_id: &str,
        _view: &str,
        _cursor_token: Option<&str>,
        _delivery_enabled: bool,
    ) -> Result<StageLogView, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }
}

impl WorkAdmissionPort for CountingPorts {
    fn submit(&self, _request: &SubmitRequest) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn cancel(&self, _request: &CancelRequest) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn accept(&self, _request: &AcceptRequest) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn resume(&self, _request: &ResumeRequest) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn policy_identity(
        &self,
        _project_alias: &str,
        _kind: &str,
        _snapshot: &serde_json::Value,
    ) -> Result<PolicyIdentity, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn policy_drifted(
        &self,
        _workflow_id: Uuid,
        _snapshot: &serde_json::Value,
    ) -> Result<bool, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }
}

impl WorkDecisionPort for CountingPorts {
    fn plan_decide(&self, _request: &PlanDecideRequest) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn review_decide(&self, _request: &ReviewDecideRequest) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn handoff_author(
        &self,
        _request: &HandoffRequest,
        _authority: &HandoffAuthority,
    ) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn repair(&self, _request: &RepairRequest) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn publication_retry(
        &self,
        _request: &PublicationRetryRequest,
    ) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn approve_external_plan_override(
        &self,
        _request: &ExternalOverrideRequest,
    ) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }

    fn recover_external_review_infrastructure(
        &self,
        _request: &ExternalRecoveryRequest,
    ) -> Result<ControlOutcome, WorkError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkError::store("stub"))
    }
}

#[derive(Default)]
struct World {
    response: Option<Response<WorkResponse>>,
    error: Option<DispatchError<WorkError>>,
    port_calls: usize,
}

fn list_payload() -> WorkPayload {
    WorkPayload::List {
        project_alias: "homelab".into(),
        status_filter: None,
        limit: 20,
        cursor_task_id: None,
        delivery_visible: false,
        task_kind_filter: None,
    }
}

fn run_dispatch(
    w: &mut World,
    header: Header,
    payload: WorkPayload,
    admission_open: bool,
) -> Result<(), String> {
    let bridge = work_bridge().map_err(|e| format!("{e:?}"))?;
    // The bridge's handlers are 'static, so the context's ports and
    // catalog must be 'static too; leak one pair per dispatch (the
    // count stays readable through the leaked reference).
    let ports: &'static CountingPorts = Box::leak(Box::new(CountingPorts::new()));
    let catalog: &'static WorkCatalog = Box::leak(Box::new(WorkCatalog::new(vec![])));
    let ctx = WorkContext::new(ports, ports, ports, catalog, admission_open);
    match bridge.dispatch(Envelope::new(header, payload), &ctx) {
        Ok(ok) => w.response = Some(ok),
        Err(err) => w.error = Some(err),
    }
    w.port_calls = ports.count();
    Ok(())
}

fn steps() -> Steps<World> {
    Steps::new()
        .given(
            "the work bridge can dispatch registered operations",
            |_, _| Ok(()),
        )
        .given(
            "the work bridge can deny traffic when admission is closed",
            |_, _| Ok(()),
        )
        .given(
            "the work bridge can verify payload kinds against headers",
            |_, _| Ok(()),
        )
        .given(
            "the work bridge can reject unregistered destinations",
            |_, _| Ok(()),
        )
        .given(
            "the work bridge can reject unsupported contract versions",
            |_, _| Ok(()),
        )
        .given(
            "the work bridge can surface failures from its read port",
            |_, _| Ok(()),
        )
        .when(
            "an admitted list envelope arrives with a correlation token",
            |w, _| {
                let header = Header::new(route_key(WorkKind::List).unwrap(), "req-list-1")
                    .unwrap()
                    .with_correlation_id("corr-bdd-1")
                    .unwrap();
                run_dispatch(w, header, list_payload(), true)
            },
        )
        .when(
            "a list envelope arrives while admission is closed",
            |w, _| {
                let header =
                    Header::new(route_key(WorkKind::List).unwrap(), "req-closed-1").unwrap();
                run_dispatch(w, header, list_payload(), false)
            },
        )
        .when("a get header carries a list payload", |w, _| {
            let header = Header::new(route_key(WorkKind::Get).unwrap(), "req-mismatch-1").unwrap();
            run_dispatch(w, header, list_payload(), true)
        })
        .when("an envelope targets an unregistered destination", |w, _| {
            let header =
                Header::new(RouteKey::new("work.unknown", 1).unwrap(), "req-unk-1").unwrap();
            run_dispatch(w, header, list_payload(), true)
        })
        .when("a list envelope requests an unsupported version", |w, _| {
            let header =
                Header::new(RouteKey::new("work.list", 99).unwrap(), "req-ver-99").unwrap();
            run_dispatch(w, header, list_payload(), true)
        })
        .when(
            "an admitted get request hits a failing read port",
            |w, _| {
                let header =
                    Header::new(route_key(WorkKind::Get).unwrap(), "req-get-fail").unwrap();
                let payload = WorkPayload::Get {
                    project_alias: "homelab".into(),
                    task_id: Uuid::nil(),
                };
                run_dispatch(w, header, payload, true)
            },
        )
        .then(
            "the response carries the matching correlation header and task page",
            |w, _| {
                let response = w.response.as_ref().ok_or("expected Ok response")?;
                let hdr = &response.header;
                let resp = &response.payload;
                ensure(
                    hdr.correlation_id() == Some("corr-bdd-1"),
                    format!("correlation was {:?}", hdr.correlation_id()),
                )?;
                ensure(
                    matches!(resp, WorkResponse::Tasks(_)),
                    format!("unexpected response: {resp:?}"),
                )?;
                ensure(
                    w.port_calls == 1,
                    format!("port_calls was {}", w.port_calls),
                )
            },
        )
        .then("dispatch is denied and no port is called", |w, _| {
            ensure(
                matches!(w.error, Some(DispatchError::Admission(_))),
                format!("error was {:?}", w.error),
            )?;
            ensure(
                w.port_calls == 0,
                format!("port_calls was {}", w.port_calls),
            )
        })
        .then(
            "dispatch rejects the mismatch and no port is called",
            |w, _| {
                ensure(
                    matches!(w.error, Some(DispatchError::PayloadMismatch(_))),
                    format!("error was {:?}", w.error),
                )?;
                ensure(
                    w.port_calls == 0,
                    format!("port_calls was {}", w.port_calls),
                )
            },
        )
        .then(
            "dispatch reports an unknown route and no port is called",
            |w, _| {
                ensure(
                    matches!(w.error, Some(DispatchError::UnknownRoute(_))),
                    format!("error was {:?}", w.error),
                )?;
                ensure(
                    w.port_calls == 0,
                    format!("port_calls was {}", w.port_calls),
                )
            },
        )
        .then(
            "dispatch returns the handler error after one port call",
            |w, _| {
                ensure(
                    matches!(w.error, Some(DispatchError::Handler(_))),
                    format!("error was {:?}", w.error),
                )?;
                ensure(
                    w.port_calls == 1,
                    format!("port_calls was {}", w.port_calls),
                )
            },
        )
}

#[test]
fn forge_work_bridge_feature() {
    let report = verify(
        include_str!("../features/forge_work_bridge.feature"),
        &steps(),
    )
    .unwrap_or_else(|errors| panic!("forge_work_bridge.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 6);
}
