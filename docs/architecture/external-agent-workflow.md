# External author and native reviewer SDLC

M22 closes the Muse database handoff through independent review, implementation,
Jenkins and exact-source merge. It preserves native P10 and old external v1
recipes; external results never manufacture a native attempt, stop proof,
reconciliation or native source approval.

```mermaid
sequenceDiagram
    participant U as Portal
    participant C as API database coordinator
    participant P as PostgreSQL
    participant M as Muse-owned runner
    participant N as Native Codex worker
    participant D as Trusted delivery worker
    participant J as Jenkins
    U->>C: Submit Plan + frozen external policy digest
    C->>P: Root task + assignment + workflow + event
    M->>P: Heartbeat / scoped claim / finish full Plan
    C->>P: Queue durable trusted source action
    D->>P: Claim action / checkpoint exact main revision
    C->>P: Queue real native review task + immutable full context
    N->>P: Claim actual lease / read bound context
    N->>N: Read-only complete Plan review
    N->>P: Publish structured verdict / actual stop / reconcile
    loop Up to three requested Plan revisions
        C->>P: Validate verdict / queue author revision context
        M->>P: Finish full revised Plan
        C->>P: Queue independent review
        N->>P: Publish and reconcile verdict
    end
    C->>P: Approved Plan / Muse Implement assignment + full context
    M->>P: Finish candidate commit/tree/base manifest
    C->>P: Queue trusted candidate preparation
    D->>D: Quarantine raw candidate / produce clean same-tree commit
    D->>P: Checkpoint clean commit/tree/diff/gate manifest
    C->>P: Queue independent native code review
    N->>N: Review full approved Plan + exact complete diff
    N->>P: Bound verdict + gate attestations / stop / reconcile
    C->>P: Queue exact-source verification action
    D->>J: Reconcile stable action ID / run fixed configured checks
    J-->>D: Actual queue/build observations and result
    D->>P: Pending or exact candidate/tree/gate SUCCESS receipt
    C->>P: Commit irreversible merge intent
    D->>D: Guarded remote main update / lost-ack reconciliation
    D->>P: Actual exact-source merge receipt
    C->>P: Complete root + same-envelope history event
```

The diagram abbreviates equivalent bounded code repair/review/CI rounds. Invalid
reviewer JSON is retried once on the same source before a human exception; it
consumes no author revision. Main movement refreshes trusted source before a
bounded rebase. Cancelled/revoked workflows retain scoped Jenkins cleanup work.
Notifications only wake discovery. Rows, generation/token fences, current grants,
immutable hashes and actual receipts determine every transition.

| Bootstrap owner | Ready port and lifetime | Effects |
| --- | --- | --- |
| API WorkServiceFactory | Owner/project-scoped ExternalWorkflowStore injected through ExternalWorkDAO and WorkflowCoordinator | PostgreSQL only; no model, Git, Jenkins or remote merge credentials |
| Native CodingWorkerFactory | Worker-owned ExternalReviewContextProvider, selected by external_reviews | Actual native lease validation, readonly full artifact staging, verified candidate workspace |
| Delivery service factory | ExternalDeliveryStoreFactory plus trusted source/prepare/verify/merge/cancel ports | Separate reconciler account owns bounded Git/Jenkins/merge effects; model receives none of its secrets |
| Muse runner | Existing scoped SQL claim/renew/progress/finish functions | Muse owns its process and compute; no Mission Control Muse CLI or email |

Configuration example (profile names are installation-specific):

```json
{
  "profiles": {
    "muse-db-primary": {"backend":"muse","model":"external-runner","resource_profile":"forge-external-db-v2","execution_enabled":true}
  },
  "external_workflows": {
    "homelab": {"author_profile":"muse-db-primary","reviewer_profile":"codex-primary","max_plan_revisions":3,"max_code_revisions":3,"auto_merge":true}
  }
}
```

Native worker configuration must explicitly set `external_reviews: true`.
M22 installs no new login. An operator enables the existing reconciler principal
in `forge_external_workflow.delivery_grants` for the configured owner/project.
Normal migrations and `verify-external-workflows.sql` pin private tables,
immutable binding triggers, scoped APIs and ACLs. Muse's existing thirteen read
grants and future-table SELECT default remain intact; private schema access and
mutation rights are not widened.

A delivery claim returns a typed action lease with `action_id`, `workflow_id`,
project, kind, generation, secret token, expiry and immutable request. The request
contains the root correlation ID and the previous checkpoint receipt. Pending
checkpoints release the lease for later observation; a worker first reconciles
Jenkins using that stable action identity rather than blindly submitting again.
Cleanup actions reference the original verify action/request/receipt and may
only stop that owned job. Cancellation is not a process-stop receipt.

For source selection, the trusted worker reports `base_revision`. Prepared
candidates include exact commit/tree/integration-base, full diff hash and
protected gate manifest/hash. Verification reports `result: SUCCESS` only after
actual checks and binds the same candidate/tree/gate manifest. Merge records the
actual merge commit and tree. None advances `refs/forge/verified-main` or creates
a native source-approval receipt; that native provider retains its original
receipt semantics.

## Jenkins execution and recovery

`deploy/mission-control/jenkins/setup-ci.py --execute` provisions the fixed
`forge-exact-candidate` job and a single WebSocket agent. The controller retains
zero executors. The Jenkins account supervises the agent; candidate checks run
as the separate `forge_ci` account with no supplementary groups, credential
mounts or privilege gain. A root-owned UUID-only supervisor verifies an immutable
bundle, source tree and protected-gate manifest before checkout, then runs exactly
`make ci`, `make ui-qa`, and the complete agent-runner Python test suite. The
candidate cannot select another command. The supervisor confirms all build-user
processes are reaped and writes a source-bound, root-owned check receipt.

`forge-external-delivery.service` selects an owner-scoped PostgreSQL DAO and
trusted delivery ports from its private typed configuration. It uses Forge's
Rust Jenkins `IoClient` through the pinned `forge` executable, rather than a
second Python HTTP adapter. Its separately selected Jenkins configuration must
enable mutations and observe only the seven public parameters: action, workflow,
commit, tree, expected main, gate-manifest digest and correlation ID. The existing
operator read-only configuration remains separate.

The admitted CLI operations are `jenkins.queue-list`, `jenkins.recent-builds`,
`jenkins.build`, `jenkins.queue`, `jenkins.stop-build` and `jenkins.cancel`, all
with an explicit `--jenkins-config` path. There are no background model or OAuth
requests in this worker. Database leases bound each action; transient transport
failure preserves the same action and its deadline. A durable journal precedes
the first Jenkins POST. Lost acknowledgements trigger observation, never a
blind second POST. Duplicate conflicting runs or incomplete observations cannot
establish success or absence. Cancellation waits for an actual cancelled queue
or a stopped build with the supervisor's cleanup receipt.

Publication imports a same-tree commit with public main as its sole parent.
Private author history stays in quarantine. After independent approval and all
three exact-source gates, the worker publishes this clean candidate and performs
a non-forced main update. Main movement requires fresh integration, review and
checks; lost merge acknowledgements are reconciled against real main ancestry.

A genuine access/scope exception or exhausted revision budget ends the workflow
as **Needs attention** with its reason and complete evidence retained. Open the
linked author/reviewer tasks from the root history to inspect the latest artifact
and findings. After correcting the access or scope issue, submit a new Plan with
the original request, preserved artifact and relevant findings. The old workflow
remains escalated with its original budget and audit history; this recovery does
not silently reset its counters, approvals or merge authority. There is currently
no public in-place Resume/Revise action for an escalated external workflow.

## Private post-fix reviewer continuation

The private `work_external_recovery.py` operator command can authorize one
source-bound reviewer continuation after a qualified fix is deployed. Its
historical immutable-context recovery remains limited to its exact setup failure.
For a malformed CLI transport failure, it requires the actual stopped and
reconciled reviewer, unchanged external Plan artifact and original roles,
current grants and policy, no competing work or explicit user cancellation,
and the exact exhausted reviewer-correction event with twenty real admissions.

The selected private grant provider holds the existing review evidence and
reads that event through its operator-owned connection. The application and
model receive no new table grants. The command records an explicit post-fix
review exception, preserves `used: 20` and the event identity, refreshes trusted
source, and queues one independent review of the retained Plan. It does not
reserve a twenty-first automatic attempt or create another failure event. A
valid review closes the same exhausted event through normal review consumption;
another failure leaves it exhausted and cannot start an automatic retry loop.
Replays return the same receipt; another exception is refused. This is a private
operator action following a human's recovery request, not a model-selected or
portal approval capability.

## Private operator plan approve-override

The private `work_external_plan_override.py` operator command implements the
single narrow `escalated -> implement` transition defined in
[external-plan-approve-override.md](proposals/external-plan-approve-override.md).
It applies only to an external `muse-db` workflow stranded in plan-revision
exhaustion (M31 20-attempt `planning` exhaustion or pre-M31 legacy plan-budget
exhaustion, with `code_revisions = 0`, no prior `approved_plan`/`candidate`/
`merge_receipt`, no user cancellation receipt, active scope/worker grants, and a
reconciled stopped reviewer `revise` verdict on the exact pinned plan artifact).

Its dedicated bootstrap-injected `PlanApproveOverrideGrantScope` is never wired
into the HTTP host's default `WorkServiceFactory` and is separate from
`NativeReviewerGrantScope`. Every execution requires root access from an
installed immutable release, explicit `--confirm-accept-as-is --execute`,
non-empty human `reason` and `acceptance_ref`, and cryptographic/revision pins
(`source_review_id`, `plan_assignment_id`, `plan_sha256`,
`expected_workflow_revision`, `expected_root_revision`). In one atomic
transaction it reactivates the root envelope, sets `approved_plan` and honest
override provenance in `plan_review` (`decision: "user_override_approve"`),
appends both an operator audit checkpoint and a coordinator `implement`
transition checkpoint on `root_task_id`, queues the `implement` assignment via
`enqueue_v2`, and writes an immutable `UNIQUE(workflow_id)` audit row in
`forge_external_workflow.plan_approve_overrides` (migration 0038, protected by
a `P0009` `BEFORE UPDATE OR DELETE` trigger). Prior plan reviews, artifacts,
and exhausted retry events remain untouched, and full independent Codex code
review, Jenkins checks, and guarded merge execute unchanged.

## Persistent per-agent branch tracking (source M39)

External author profiles (such as `muse-db-primary`) resolve to a single
persistent non-`main` branch (`forge/agent/muse-db-primary` by default, or an
explicit startup-validated override such as `feature/muse`). External delivery
enforces the same exclusive active-workflow ownership contract through
`forge_external_delivery_api.agent_branch_acquire`, `agent_branch_verify_owner`,
`agent_branch_upsert`, and `agent_branch_release`, verifying manifest
`source_branch`, tracking trunk `main` fetch SHA/timestamps and Jenkins cache
status in `forge_agent.agent_branch_state`, and projecting both branch and
post-merge Jenkins stages under the workflow's `correlation_id`. Live rollout
of migration 0039 and delivery services remains a separate authorized step.
