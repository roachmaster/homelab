use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro reader-intro"];
        element!(H1; text("Agent feedback and repair")),
        element!(P; text("The original agent gets a verified opportunity to repair its work before coordinator takeover. Activation and deployed policy remain configuration controlled.")),
        element!(Nav [Class = "section-nav", AriaLabel = "Feedback loop sections"];
            element!(A [Href = "#repair-flow"]; text("Repair flow")),
            element!(A [Href = "#repair-boundaries"]; text("Ownership")),
            element!(A [Href = "#repair-handoff"]; text("Handoff rules")),
            element!(A [Href = "/design/coding-agents"]; text("Coding-agent dispatch")),
        ),
    ),
    element!(Section [Id = "repair-flow", Class = "design-section", AriaLabelledby = "repair-flow-title"];
        element!(H2 [Id = "repair-flow-title"]; text("From candidate to verified repair")),
        element!(Ol [Class = "contract-steps"];
            element!(Li;
                element!(Strong; text("Freeze the candidate.")),
                text(" Stop the previous process scope, retain its result and reconcile the exact source. A new writer cannot begin until ownership and cleanup are confirmed."),
            ),
            element!(Li;
                element!(Strong; text("Run independent checks.")),
                text(" The verifier records the candidate commit, required check manifest, actual toolchain and bounded diagnostic evidence. A model's completion message does not close a finding."),
            ),
            element!(Li;
                element!(Strong; text("Deliver source-bound feedback.")),
                text(" Persist stable finding IDs, reproduction, expected behavior and acceptance checks in PostgreSQL. The worker reads the authorized feedback separately from the immutable approved plan and mounts it read-only for the next attempt."),
            ),
            element!(Li;
                element!(Strong; text("Let the agent repair.")),
                text(" The selected agent changes the approved workspace, reports addressed findings and focused check results, then stops. The verifier checks those claims before closing findings or scheduling another round."),
            ),
            element!(Li;
                element!(Strong; text("Review before publication.")),
                text(" A local pass pauses for the owner. Inspect the exact candidate and diagnostics, explicitly attest any resolved findings and approve the result. Authorizing automatic repair never grants automatic approval."),
            ),
            element!(Li;
                element!(Strong; text("Publish and finish the gates.")),
                text(" Trusted providers publish the checked feature branch and draft PR. Required hosted checks must pass on that exact head before final acceptance. Merge is a separate review decision."),
            ),
        ),
    ),
    element!(Section [Id = "repair-boundaries", Class = "design-section", AriaLabelledby = "repair-boundaries-title"];
        element!(H2 [Id = "repair-boundaries-title"]; text("Clear authority and ownership")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Owner and admitted Work service")),
                element!(Dd; text("Approve the plan, repair policy and any scope change. Revision, idempotency, project and artifact checks happen before queue effects.")),
            ),
            element!(Div;
                element!(Dt; text("Gemini or selected coding agent")),
                element!(Dd; text("Own the repair attempt and report claimed fixes. It receives bounded redacted diagnostics and no database, publication or provider-account credentials.")),
            ),
            element!(Div;
                element!(Dt; text("Independent verifier and review policy")),
                element!(Dd; text("Close findings only from matching evidence, distinguish source failures from infrastructure faults, and preserve the required gates. A changed check policy requires independent review.")),
            ),
            element!(Div;
                element!(Dt; text("Factories, scoped providers and PostgreSQL")),
                element!(Dd; text("Bootstrap selects implementations and injects ready ports. PostgreSQL owns attempts, leases, feedback and append-only outcomes; notifications wake workers. Workers do not query global registries.")),
            ),
            element!(Div;
                element!(Dt; text("Browser")),
                element!(Dd; text("Rust prepares guarded actions and projects round, finding and verification state. Generated JavaScript renders and relays I/O; it does not authorize repair or advance the workflow.")),
            ),
        ),
    ),
    element!(Section [Id = "repair-handoff", Class = "design-section", AriaLabelledby = "repair-handoff-title"];
        element!(H2 [Id = "repair-handoff-title"]; text("Repair, retry or hand off")),
        element!(Dl [Class = "storage"];
            element!(Div;
                element!(Dt; text("Source or configuration failure")),
                element!(Dd; text("Return the exact finding to the original agent within the authorized round and time budget. Record the actual compiler version; a different local toolchain cannot establish hosted success.")),
            ),
            element!(Div;
                element!(Dt; text("Infrastructure failure")),
                element!(Dd; text("Retry the identical artifact only within the infrastructure budget after cleanup proof. A broken verifier, credential problem or unavailable required tool belongs to the operator.")),
            ),
            element!(Div;
                element!(Dt; text("No progress or budget exhausted")),
                element!(Dd; text("The initial policy allows up to three repair rounds and hands off after two successive rounds without verified progress. Preserve the candidate, receipts, open findings and takeover reason.")),
            ),
            element!(Div;
                element!(Dt; text("Quota, cancellation or uncertain ownership")),
                element!(Dd; text("Preserve the checkpoint and stop admission. Confirm the process scope stopped before transferring the writer. Unknown allowance remains unknown; account rotation and paid-credit fallback are not automatic.")),
            ),
            element!(Div;
                element!(Dt; text("A completed task needs a later correction")),
                element!(Dd; text("Start an explicitly linked repair from the delivered candidate. Preserve the completed parent's history and source; do not resume an older workspace that omits reviewed changes.")),
            ),
        ),
    ),
];
