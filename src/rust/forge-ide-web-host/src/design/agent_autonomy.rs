use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro reader-intro"];
        element!(H1; text("Autonomous agent workflow")),
        element!(P; text("Configure an author and an independent reviewer once per project. A submitted assignment then advances through plan, implementation, review, required checks and trusted merge without routine approval prompts. Task lists and readers display the authoritative workflow phase; human action is reserved for cancellation and escalated exceptions.")),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "autonomy-flow-title"];
        element!(H2 [Id = "autonomy-flow-title"]; text("The normal workflow")),
        element!(Ol [Class = "contract-steps"];
            element!(Li; element!(Strong; text("Author a plan.")), text(" The configured native author or external Muse runner receives the assignment and acceptance criteria through its admitted provider.")),
            element!(Li; element!(Strong; text("Review the exact plan.")), text(" The independent read-only reviewer, initially Codex, returns a structured approval or concrete revision findings bound to that artifact. PostgreSQL commits the result and admits the next stage.")),
            element!(Li; element!(Strong; text("Revise within the frozen budget.")), text(" The author receives the reviewer findings. Up to three plan revisions are allowed by the initial policy; unresolved findings then escalate to the operator.")),
            element!(Li; element!(Strong; text("Implement and review the candidate.")), text(" An approved plan automatically starts implementation. Independent code review and fixed verification checks return source-bound findings to the author within the configured code-revision budget.")),
            element!(Li; element!(Strong; text("Publish, check and merge.")), text(" Trusted providers publish the exact reviewed candidate. The external database workflow requires the fixed Jenkins gates to pass on that exact commit/tree/gate manifest before the trusted merger advances main and records the remote receipt. Native workflows retain their own frozen verification provider. The agent cannot push or merge.")),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "autonomy-boundaries-title"];
        element!(H2 [Id = "autonomy-boundaries-title"]; text("Clear role boundaries")),
        element!(Dl [Class = "storage"];
            element!(Div; element!(Dt; text("Project configuration")), element!(Dd; text("Select catalog-backed author and reviewer profiles, revision budgets and merge policy. Each assignment pins the exact policy digest. A future registered agent can fill a role without hardcoding its name into the portal."))),
            element!(Div; element!(Dt; text("Author")), element!(Dd; text("Own the plan and implementation changes. Receive bounded review findings, preserve scope and return a stopped candidate for independent checks."))),
            element!(Div; element!(Dt; text("Read-only reviewer")), element!(Dd; text("Inspect the exact plan or candidate under a separate lease. Return structured approval, revision or escalation evidence; do not edit source, authorize a different artifact or access merge credentials."))),
            element!(Div; element!(Dt; text("Controller and trusted providers")), element!(Dd; text("PostgreSQL commits state and wakes injected workers. Factories supply ready context, verification, publication and merge ports. Only verified peer decisions and required check receipts admit successors."))),
            element!(Div; element!(Dt; text("Portal")), element!(Dd; text("Rust displays the phase, configured roles, separate plan/code revision counts and escalation reason. Generated JavaScript renders and relays actions. It does not infer approval from model prose or advance the workflow."))),
        ),
    ),
    element!(Section [Class = "design-section", AriaLabelledby = "autonomy-exceptions-title"];
        element!(H2 [Id = "autonomy-exceptions-title"]; text("Human exceptions and retained history")),
        element!(P; text("Cancel remains available only while the admitted workflow permits it. Exhausted revision budgets, quota/authentication stops, changed scope, conflicting remote state or uncertain cleanup preserve the artifacts and expose an escalation reason. Recovery is explicit and source-bound; ordinary Plan Resume, Approve and Accept controls are not part of this autonomous path.")),
        element!(P; text("Legacy/manual assignments retain their historical policy and applicable controls. Eligible external-v1 Plans are adopted through a real M22 binding and independent native review; an artifact alone never becomes approval. Missing autonomous context cannot fall back to manual approval. A workflow is shown as complete only after its trusted merge receipt is recorded, not when an author says the task is finished.")),
        element!(P; element!(A [Href = "/design/coding-agents#external-workflow"]; text("Muse database workflow, factories and task history →"))),
        element!(A [Href = "/design/agent-feedback-loop"]; text("Review the underlying feedback and repair boundaries →")),
        element!(P;
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/docs/architecture/proposals/forge-autonomous-agent-workflow.md"]; text("Sequence and component diagram source →")),
            text(" The Markdown source includes the diagrams; no rendered diagram asset is implied."),
        ),
    ),
];
