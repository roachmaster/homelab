/// The kind of actor who performed an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Operator,
    Coordinator,
    Worker,
    Agent,
    Service,
    System,
}

impl ActorKind {
    /// Convert `ActorKind` to a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            ActorKind::Operator => "operator",
            ActorKind::Coordinator => "coordinator",
            ActorKind::Worker => "worker",
            ActorKind::Agent => "agent",
            ActorKind::Service => "service",
            ActorKind::System => "system",
        }
    }

    /// Parse a string into an `ActorKind`.
    pub fn parse(s: &str) -> Option<ActorKind> {
        match s {
            "operator" => Some(ActorKind::Operator),
            "coordinator" => Some(ActorKind::Coordinator),
            "worker" => Some(ActorKind::Worker),
            "agent" => Some(ActorKind::Agent),
            "service" => Some(ActorKind::Service),
            "system" => Some(ActorKind::System),
            _ => None,
        }
    }
}

/// An actor, identified by a kind and an ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub kind: ActorKind,
    pub id: String,
}

impl Actor {
    /// Create a new `Actor`.
    pub fn new(kind: ActorKind, id: impl Into<String>) -> Actor {
        Actor {
            kind,
            id: id.into(),
        }
    }
}

/// The cause of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    LeaseLost,
    DeadlineExceeded,
    ScopeStopped,
    Interrupted,
    OperatorRequest,
    WorkflowCancelled,
    BudgetExhausted,
    Superseded,
    MalformedOutput,
    PolicyUnavailable,
    ContextUnavailable,
    ResourceBound,
    AgentInput,
    AgentFailed,
}

impl Cause {
    /// Convert `Cause` to a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Cause::LeaseLost => "lease_lost",
            Cause::DeadlineExceeded => "deadline_exceeded",
            Cause::ScopeStopped => "scope_stopped",
            Cause::Interrupted => "interrupted",
            Cause::OperatorRequest => "operator_request",
            Cause::WorkflowCancelled => "workflow_cancelled",
            Cause::BudgetExhausted => "budget_exhausted",
            Cause::Superseded => "superseded",
            Cause::MalformedOutput => "malformed_output",
            Cause::PolicyUnavailable => "policy_unavailable",
            Cause::ContextUnavailable => "context_unavailable",
            Cause::ResourceBound => "resource_bound",
            Cause::AgentInput => "agent_input",
            Cause::AgentFailed => "agent_failed",
        }
    }

    /// Parse a string into a `Cause`.
    pub fn parse(s: &str) -> Option<Cause> {
        match s {
            "lease_lost" => Some(Cause::LeaseLost),
            "deadline_exceeded" => Some(Cause::DeadlineExceeded),
            "scope_stopped" => Some(Cause::ScopeStopped),
            "interrupted" => Some(Cause::Interrupted),
            "operator_request" => Some(Cause::OperatorRequest),
            "workflow_cancelled" => Some(Cause::WorkflowCancelled),
            "budget_exhausted" => Some(Cause::BudgetExhausted),
            "superseded" => Some(Cause::Superseded),
            "malformed_output" => Some(Cause::MalformedOutput),
            "policy_unavailable" => Some(Cause::PolicyUnavailable),
            "context_unavailable" => Some(Cause::ContextUnavailable),
            "resource_bound" => Some(Cause::ResourceBound),
            "agent_input" => Some(Cause::AgentInput),
            "agent_failed" => Some(Cause::AgentFailed),
            _ => None,
        }
    }

    /// Check if a cause is allowed for a given event kind.
    pub fn allowed_for(&self, kind: crate::work_events::EventKind) -> bool {
        match kind {
            crate::work_events::EventKind::NeedsReconcile => matches!(
                self,
                Cause::LeaseLost
                    | Cause::DeadlineExceeded
                    | Cause::ScopeStopped
                    | Cause::Interrupted
            ),
            crate::work_events::EventKind::Cancelled
            | crate::work_events::EventKind::CancelRequested => matches!(
                self,
                Cause::OperatorRequest
                    | Cause::WorkflowCancelled
                    | Cause::BudgetExhausted
                    | Cause::Superseded
            ),
            crate::work_events::EventKind::Failed => matches!(
                self,
                Cause::MalformedOutput
                    | Cause::PolicyUnavailable
                    | Cause::ContextUnavailable
                    | Cause::ResourceBound
                    | Cause::AgentInput
                    | Cause::AgentFailed
            ),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_actor_kind_round_trip() {
        let kinds = [
            ActorKind::Operator,
            ActorKind::Coordinator,
            ActorKind::Worker,
            ActorKind::Agent,
            ActorKind::Service,
            ActorKind::System,
        ];

        for kind in kinds {
            assert_eq!(ActorKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn test_cause_round_trip() {
        let causes = [
            Cause::LeaseLost,
            Cause::DeadlineExceeded,
            Cause::ScopeStopped,
            Cause::Interrupted,
            Cause::OperatorRequest,
            Cause::WorkflowCancelled,
            Cause::BudgetExhausted,
            Cause::Superseded,
            Cause::MalformedOutput,
            Cause::PolicyUnavailable,
            Cause::ContextUnavailable,
            Cause::ResourceBound,
            Cause::AgentInput,
            Cause::AgentFailed,
        ];

        for cause in causes {
            assert_eq!(Cause::parse(cause.as_str()), Some(cause));
        }
    }

    #[test]
    fn test_allowed_for() {
        assert!(Cause::LeaseLost.allowed_for(crate::work_events::EventKind::NeedsReconcile));
        assert!(Cause::OperatorRequest.allowed_for(crate::work_events::EventKind::Cancelled));
        assert!(Cause::MalformedOutput.allowed_for(crate::work_events::EventKind::Failed));
        assert!(!Cause::BudgetExhausted.allowed_for(crate::work_events::EventKind::Failed));
    }
}
