# Design: Authorized Plan Approve-Override for External (`muse-db`) Workflows

**Status: implemented (Phase 1 private operator surface), 2026-11-24. Migration
`0038_external_plan_approve_override.sql`, `work_external_plan_override.py`,
bootstrap provider wiring, installer publication, protected-path coverage, and
disposable PostgreSQL verification tests are implemented per this approved
design. Phase 2 optional Portal HTTP surface remains unimplemented until
explicitly requested.**

This design defines a single, narrow, audited operator contract—`ExternalPlanApproveOverride`—that allows an external `muse-db` workflow stranded in plan-revision exhaustion (e.g., `phase = 'escalated'`, `enabled = false`, last plan review `status = 'revise'`, planning retry event `used = 20 / exhausted`) to proceed to `implement` when the human owner has explicitly accepted the latest plan artifact as-is.

It fits directly into the existing external workflow schema ([0022_external_workflows.sql](../../../deploy/mission-control/sql/migrations/0022_external_workflows.sql)), `muse-db` transport ([0020_external_agent_transport.sql](../../../deploy/mission-control/sql/migrations/0020_external_agent_transport.sql)), retry-event ledger ([0031_retry_events.sql](../../../deploy/mission-control/sql/migrations/0031_retry_events.sql)), event attribution rules ([work_events.py](../../../deploy/mission-control/agent-runner/work_events.py)), and private operator recovery pattern ([work_external_recovery.py](../../../deploy/mission-control/agent-runner/work_external_recovery.py), [work_retry_continuation.py](../../../deploy/mission-control/agent-runner/work_retry_continuation.py)).

---

## 1. Exact Override Operation

### Command Contract (`ExternalPlanApproveOverride`)
A single, hardcoded operation—no `target_phase` or generic state-mutation arguments:

| Field | Type / Constraint | Purpose |
| --- | --- | --- |
| `project_alias` | Identifier (`[A-Za-z0-9_.-]{1,64}`) | Scoped managed external-workflow project |
| `workflow_id` | UUID | Target workflow in `forge_external_workflow.workflows` |
| `root_task_id` | UUID | Bound root envelope task in `forge_agent.coding_tasks` |
| `source_review_id` | UUID | Latest plan review row (`status = 'revise'`) that preceded exhaustion |
| `plan_assignment_id` | UUID | Exact `forge_external_agent.assignments.assignment_id` of the accepted plan |
| `plan_sha256` | 64 lowercase hex | Exact SHA-256 of the immutable plan artifact accepted by the user |
| `expected_workflow_revision` | `int` (`1..2^53-1`) | Optimistic concurrency pin on `workflows.revision` |
| `expected_root_revision` | `int` (`1..2^53-1`) | Optimistic concurrency pin on root `coding_tasks.revision` |
| `idempotency_key` | UUID / `[A-Za-z0-9_.-]{1,128}` | Replay-safe receipt key |
| `reason` | Non-empty UTF-8 (`1..2048` B, no control chars) | Explicit human explanation for accepting the plan as-is |
| `acceptance_ref` | Non-empty UTF-8 (`1..256` B, no control chars) | Reference to the explicit user acceptance (e.g., chat/decision ref or portal confirmation token) |
| `confirm_accept_as_is` | Literal `True` | Explicit opt-in attestation; cannot default to `True` |

### Strict Entry Preconditions (Checked Atomically Under Row Locks)
Inside one transaction locking `workflows`, `root` task, `source` plan task, `reviews` (`FOR UPDATE`), and holding the private operator grant lock (`FOR SHARE` on `coding_worker_grants`, `coding_attempts`, `coding_reconciliation_receipts`, `coding_retry_events`, `coding_retry_event_observations`):

1. **Active policy & grants**: Project is configured in `external_workflows`; `author_profile`, `reviewer_profile`, and `policy_digest` match the frozen workflow `snapshot`; both profiles have `execution_enabled = true`; `forge_external_workflow_api.service_scope(owner, project, author_profile, active=true)` succeeds.
2. **Exact stranded state**:
   - `w.phase = 'escalated'`, `w.enabled = false`, `w.lease_token IS NULL`, `w.lease_until IS NULL`.
   - `w.code_revisions = 0`; `w.state` contains valid 40-hex `base_revision` and `active_review_id == source_review_id`, and contains **none** of `approved_plan`, `candidate`, or `merge_receipt`.
   - No row in `forge_agent.coding_control_receipts` with `action = 'cancel'` on `root_task_id` (proves system escalation, not user cancellation).
3. **Verified plan-revision exhaustion**:
   - `forge_external_workflow.reviews` row for `source_review_id` has `target_kind = 'plan'`, `status = 'revise'`, `verdict->>'decision' = 'revise'`, `receipt->>'decision' = 'revise'`, valid `target_json`/`target_sha256` binding `source_task_id == w.active_task_id`, `source_assignment_id == plan_assignment_id`, `source_sha256 == plan_sha256`, and a reconciled stopped native reviewer attempt with valid systemd stop proof (matching [work_retry_continuation.py](../../../deploy/mission-control/agent-runner/work_retry_continuation.py#L130-L157)).
   - Exhaustion matches **either**:
     - Current retry-event model: `forge_agent.coding_retry_events` row for `(scope_kind='external_workflow', scope_id=workflow_id, kind='planning', phase_id='planning')` has `status = 'exhausted'`, `used = 20`, `attempt_limit = 20`, 20 real rows in `coding_retry_event_admissions` (or adopted `legacy_used` plus real admissions totaling 20), and a failure observation for `review:<source_review_id>`; **or**
     - Pre-M31 legacy budget exhaustion: `w.state->>'escalation_reason'` starts with `'The plan revision budget is exhausted. '` and `w.plan_revisions >= w.snapshot->>'max_plan_revisions'`.
4. **Immutable plan artifact integrity**: `forge_external_agent_api.artifact_by_task(owner, project, source_task_id)` matches `plan_assignment_id`, `plan_sha256`, `review.source_sha256`, `target['source_content']`, and recomputed `sha256(content)`.
5. **Quiescence**: Zero non-stopped/leased `coding_attempts`, zero `queued`/`leased` `forge_external_workflow.actions`, and zero `queued`/`running`/`cancel_requested` `coding_tasks` linked to `workflow_id`.
6. **Optimistic concurrency & one-shot idempotency**:
   - If a row already exists in `forge_external_workflow.plan_approve_overrides` for `workflow_id`: return the stored `receipt` on identical `(idempotency_key, intent_digest)`; reject with `409 idempotency_conflict` on same key with different intent; reject with `409 not_reviewable` on any new key (one override per workflow maximum).
   - Otherwise require `w.revision == expected_workflow_revision` and `root.revision == expected_root_revision` (`409 revision_conflict` on mismatch).

### State Diff: What Changes vs. What Does Not

| Entity | Changes (in one atomic transaction) | Explicitly Unchanged (Immutable / Preserved) |
| --- | --- | --- |
| `forge_external_workflow.plan_approve_overrides` *(proposed)* | `INSERT` 1 immutable audit row (`UNIQUE(workflow_id)`). | Rows can never be `UPDATE`d or `DELETE`d (`P0009` trigger). |
| `forge_external_workflow.workflows` | `phase: 'escalated' -> 'implement'`, `enabled: false -> true`, `active_task_id -> <new implement task_id>`, `revision: revision + 1`, `state` adds `approved_plan`, honest `plan_review` (override provenance), and `plan_approve_override` summary; pops `active_review_id` and `escalation_reason`. | `owner_id`, `project_alias`, `root_task_id`, `author_profile`, `reviewer_profile`, `policy_digest`, `snapshot`, `created_at`, `plan_revisions` (stays `20`), `code_revisions` (stays `0`), `base_revision`. |
| `forge_agent.coding_tasks` (root envelope) | `status: 'cancelled' -> 'awaiting_review'`, `cancelled_at -> NULL`, `revision: revision + 1`. | `correlation_id`, `prompt`, `acceptance_criteria`, `pinned_recipe`, `recipe_digest`, `created_at`. |
| `forge_agent.coding_tasks` + `forge_external_workflow.tasks` (new implement child) | `INSERT` new `mode = 'implement'` task (`role = 'author'`, frozen `author_implement` recipe, `status = 'queued'`, `revision = 1`) + sequence-1 `queued` event. | All prior plan author tasks and native reviewer tasks remain untouched. |
| `forge_external_agent.assignments` + `forge_external_workflow.contexts` | `enqueue_v2()` inserts new `implement` assignment (`status = 'queued'`) and emits `pg_notify('forge_external_agent_jobs', '')`; `INSERT` immutable `contexts` row for the new assignment. | Prior plan `assignments`, `artifacts`, and `contexts` are completely untouched. |
| `forge_agent.coding_events` (on `root_task_id`) | Append two events (same `correlation_id`, monotonic `sequence`): (1) operator override audit event, (2) coordinator `implement` phase checkpoint linking the new `implement` task. | All prior events—including all 20 revise rounds, cancellation events, and the escalation checkpoint—remain untouched. |
| `forge_external_workflow.reviews` | **None.** | Latest plan review remains `status = 'revise'` with its original `verdict`, `receipt`, `reviewer_attempt_id`, and `reviewer_result_sha256`. |
| `forge_agent.coding_retry_events` & admissions | **None.** | Planning retry event remains `used = 20`, `attempt_limit = 20`, `status = 'exhausted'`, with all 20 admission rows and observations intact. |

---

## 2. Authorization & Explicit User Acceptance Verification

### Who Can Invoke It
- **Authorized**: Only the authenticated human owner/operator of the scoped project.
- **Forbidden**: External Muse runners (`muse-db` / `muse_events`), native workers (`forge_coding_worker`), delivery workers (`forge_coding_reconciler`), LLM/model tool calls, and the automatic coordinator (`ExternalWorkflowStore.run_once()`). None of these principals receive execute/insert grants or port references for the override.

### Wiring & Invocation Surfaces (Following `wire-forge-providers`)
1. **Primary surface (Phase 1 — smallest blast radius, matches [work_external_recovery.py](../../../deploy/mission-control/agent-runner/work_external_recovery.py))**:
   - Private operator CLI (`work_external_plan_override.py`, or subcommand alongside `work_external_recovery.py`) running from an installed immutable release under operator host access, acquiring real peer-authenticated `forge_agent` + read/lock `postgres` connections.
   - `ExternalWorkflowStore.approve_plan_override(command)` requires a dedicated bootstrap-injected private grant provider (`plan_override_grant_provider`, or an explicit capability on `recovery_grant_provider`). The HTTP host's normal `WorkServiceFactory` does **not** inject this provider, so no HTTP route or worker can reach it in Phase 1.
2. **Optional Portal surface (Phase 2, if later requested)**:
   - Admitted through Forge's single header-routed front door with a dedicated destination (`X-Forge-Destination: ide.work.external.plan.approve_override`), authenticated strictly to the session `owner_id` (never caller-supplied).
   - Existing `forbid_manual_control()` in [work_api.py](../../../deploy/mission-control/agent-runner/work_api.py#L345-L348) stays unchanged for `ide.work.accept`, `ide.work.resume`, `ide.work.plan.decide`, and `ide.work.review.decide`.

### How Explicit User Acceptance Is Verified
The override rejects blind workflow-ID approvals. Acceptance is proven by five combined checks:
1. **Transport/OS-bound actor**: `actor = {"kind": "operator", "id": <authenticated owner>}` is derived solely from the scoped store/OS/session identity—never trusted from payload fields.
2. **Content-addressed artifact pin**: The caller must pass the exact `plan_assignment_id` and 64-hex `plan_sha256` of the plan artifact they reviewed; both are cross-checked against `forge_external_agent.artifacts` and `forge_external_workflow.reviews`.
3. **Review & state pin**: `source_review_id`, `expected_workflow_revision`, and `expected_root_revision` prove the operator observed the exact final `revise` verdict and current workflow revision.
4. **Explicit acceptance attestation**: Non-empty `reason`, non-empty `acceptance_ref`, and explicit `confirm_accept_as_is = True` (CLI requires `--confirm-accept-as-is --execute`; portal Phase 2 requires an explicit confirmation modal echoing the plan digest and capturing user reason).
5. **Reconciled, non-cancelled evidence**: Verified no user `cancel` receipt exists on `root_task_id` and all prior reviewer attempts are stopped and reconciled.

---

## 3. Immutable Audit Record & Visibility

### Authoritative Audit Table (`forge_external_workflow.plan_approve_overrides`)
Proposed in a future migration (e.g., `0038_external_plan_approve_override.sql`, with matching catalog verification update in [verify-external-workflows.sql](../../../deploy/mission-control/sql/management/verify-external-workflows.sql)):

- **Columns**:
  - Identity & scope: `override_id uuid PRIMARY KEY DEFAULT gen_random_uuid()`, `owner_id text NOT NULL`, `project_alias text NOT NULL`, `workflow_id uuid NOT NULL UNIQUE`, `root_task_id uuid NOT NULL`.
  - Actor & acceptance: `actor_kind text NOT NULL CHECK (actor_kind = 'operator')`, `actor_id text NOT NULL`, `reason text NOT NULL CHECK (octet_length(reason) BETWEEN 1 AND 2048 AND btrim(reason) <> '')`, `acceptance_ref text NOT NULL CHECK (octet_length(acceptance_ref) BETWEEN 1 AND 256 AND btrim(acceptance_ref) <> '')`, `created_at timestamptz NOT NULL DEFAULT clock_timestamp()`.
  - Bound plan artifact: `plan_task_id uuid NOT NULL`, `plan_assignment_id uuid NOT NULL REFERENCES forge_external_agent.assignments(assignment_id)`, `plan_sha256 text NOT NULL CHECK (plan_sha256 ~ '^[0-9a-f]{64}$')`.
  - Preserved prior state: `prior_workflow_revision bigint NOT NULL`, `prior_root_revision bigint NOT NULL`, `prior_phase text NOT NULL CHECK (prior_phase = 'escalated')`, `prior_plan_revisions integer NOT NULL`, `prior_review_id uuid NOT NULL REFERENCES forge_external_workflow.reviews(review_id)`, `prior_escalation_reason text NOT NULL`, `retry_event_id uuid REFERENCES forge_agent.coding_retry_events(event_id)`.
  - Replay & handoff binding: `idempotency_key text NOT NULL`, `intent_digest text NOT NULL CHECK (intent_digest ~ '^[0-9a-f]{64}$')`, `implement_task_id uuid NOT NULL`, `implement_assignment_id uuid NOT NULL REFERENCES forge_external_agent.assignments(assignment_id)`, `receipt jsonb NOT NULL`.
- **Immutability & ACLs**:
  - `BEFORE UPDATE OR DELETE` trigger raising `ERRCODE = 'P0009'` (`Immutable external plan approve override`).
  - `REVOKE ALL FROM PUBLIC`; `GRANT SELECT, INSERT ON forge_external_workflow.plan_approve_overrides TO forge_agent` (no `UPDATE`, no `DELETE`); zero write grants to worker, reconciler, or external runner roles.

### Where the Audit Record Is Visible
1. **PostgreSQL audit log**: Direct query on `forge_external_workflow.plan_approve_overrides` joined to `workflows`, `reviews`, and `forge_external_agent.artifacts`.
2. **Task event stream (`forge_agent.coding_events` -> `ide.work.events`)**:
   - Emitted via [work_events.py](../../../deploy/mission-control/agent-runner/work_events.py#L84) `insert_event()` on `root_task_id` (preserving `correlation_id`):
     - **Event N+1 (operator audit event)**: `event_kind = 'checkpoint'`, `actor = {"kind": "operator", "id": owner}`, payload `{stage: "plan_approve_override", override_id, workflow_id, prior_phase: "escalated", prior_review_id, prior_escalation_reason, plan_revisions, retry_event_id, plan_assignment_id, plan_sha256, reason, acceptance_ref, implement_task_id, summary}`. Readable by operators and `muse_events` (which already has `SELECT` on `forge_agent.coding_events`).
     - **Event N+2 (coordinator transition checkpoint)**: `event_kind = 'checkpoint'`, `actor = COORDINATOR_ACTOR`, exact `_public_workflow_progress` shape `{workflow_id, phase: "implement", related_task_id: implement_task_id, summary: "Plan approved by operator override <override_id> (<owner>, plan <sha256[:12]>): <reason>. Implementation queued."}`.
   - **Strict DTO compatibility**: Because `WorkWorkflowSummary` ([workflow.rs](../../../src/rust/forge-ide-ui/src/protocol/work/workflow.rs#L65-L90)) and `WorkLogEvent` ([work.rs](../../../src/rust/forge-ide-ui/src/protocol/work.rs#L982-L997)) use `#[serde(deny_unknown_fields)]`, Event N+2 makes the override immediately visible in the existing Portal Agents timeline and stage diagram with zero wire-ABI break in Phase 1. In a follow-up UI slice, adding optional `plan_approve_override` fields to `WorkLogEvent` and `WorkWorkflowSummary` (with a matching `_public_plan_approve_override` validator in [work_store.py](../../../deploy/mission-control/agent-runner/work_store.py#L117-L138)) exposes structured override badges in the task header and history.
3. **Downstream delivery action intents (`forge_external_workflow.actions`)**:
   - `w.state['plan_review']` is carried into `verify` and `merge` intents by `_delivery_request(w)` ([work_external_workflow.py](../../../deploy/mission-control/agent-runner/work_external_workflow.py#L713-L715)). Recording `decision: "user_override_approve"` with `override_id`, `actor`, `reason`, `approved_at`, `plan_assignment_id`, `plan_sha256`, and `prior_review_id` keeps the override permanently visible in every downstream delivery receipt without ever claiming Codex approved the plan.

---

## 4. History Preservation

- **No rewritten verdicts**: The final plan review in `forge_external_workflow.reviews` stays `status = 'revise'` with its authentic Codex verdict, findings, attempt ID, and reconciliation receipt. No synthetic `'approved'` review row is inserted.
- **No reset retry counters**: `workflows.plan_revisions` stays `20`, and `forge_agent.coding_retry_events` (`kind = 'planning'`, `used = 20`, `status = 'exhausted'`) plus all 20 `coding_retry_event_admissions` rows stay intact. The override deliberately does **not** call `retry_event_resolve()` because no reviewer approval occurred; downstream `implement`/`code_review`/`checks`/`merge` stages use distinct retry kinds (`review`, `compilation`, `tests`, `verification`, `merge`, `feedback_delivery`), so the retained `planning` event never blocks implementation or CI repair budgets.
- **Additive timeline**: Every prior `coding_events` row (revise rounds 1..20, escalation checkpoint, child cancellation events) stays unmodified; the override appears strictly as new monotonic events after escalation on the same `correlation_id`.
- **Honest provenance**: `w.state['plan_review']` uses `decision: 'user_override_approve'` (distinct from reviewer `'approve'`) and links directly to `prior_review_id` (`prior_decision: 'revise'`), preserving the distinction between automated peer approval and human override acceptance.

---

## 5. Implement Handoff After Override

The override hands off directly into the existing `muse-db` + M22 implementation pipeline in one atomic transaction—requiring **zero** changes to the Muse runner protocol (`forge_external_agent_api.claim` / `finish`) or delivery worker ([work_external_delivery.py](../../../deploy/mission-control/agent-runner/work_external_delivery.py)):

```mermaid
sequenceDiagram
    participant Op as Authenticated Owner/Operator
    participant Flow as ExternalWorkflowStore.approve_plan_override
    participant DB as PostgreSQL (atomic txn)
    participant Muse as External Muse Runner
    participant Coord as Workflow Coordinator + Delivery Worker

    Op->>Flow: ExternalPlanApproveOverride (plan_assignment_id, plan_sha256, source_review_id, revisions, reason, acceptance_ref)
    Flow->>DB: Lock workflow, root, plan task, review + private grant/retry locks
    Flow->>DB: Verify escalated + plan revise/exhausted + artifact digest + quiescence
    Flow->>DB: Reactivate root task; set approved_plan & override plan_review; phase=implement, enabled=true
    Flow->>DB: Insert new implement task + append operator audit & coordinator events
    Flow->>DB: forge_external_workflow_api.enqueue_v2() -> assignments + pg_notify + contexts
    Flow->>DB: INSERT immutable forge_external_workflow.plan_approve_overrides
    DB-->>Op: Durable override receipt (override_id, implement_task_id, implement_assignment_id)
    Muse->>DB: forge_external_agent_api.claim(project, author_profile)
    DB-->>Muse: implement assignment + external_context (approved_plan, base_revision, result_contract)
    Muse->>DB: forge_external_agent_api.finish(candidate manifest)
    Coord->>DB: Normal prepare (with main_changed rebase if needed) -> Codex code_review -> Jenkins checks -> guarded merge
```

Key handoff details:
1. **Satisfies SQL `enqueue_v2` gate naturally**: [0022_external_workflows.sql](../../../deploy/mission-control/sql/migrations/0022_external_workflows.sql#L130) allows `enqueue_v2` for `mode = 'implement'` only when `w.enabled AND w.phase IN ('implement','repair') AND w.active_task_id = t.id AND w.state ? 'approved_plan'`. Persisting `approved_plan = {assignment_id, sha256, content}` and `phase = 'implement'`, `enabled = true` before invoking `enqueue_v2` keeps that SQL gate completely unchanged.
2. **Exact Muse claim payload**: Reusing `_queue_author(cursor, w, 'implement', artifact, None)` ([work_external_workflow.py](../../../deploy/mission-control/agent-runner/work_external_workflow.py#L696-L711)) writes the canonical v2 `forge_external_workflow.contexts` row (`approved_plan`, `previous_artifact`, `base_revision`, `request_prompt`, `acceptance_criteria`, `result_contract`, `feedback: None`) and emits `pg_notify('forge_external_agent_jobs', '')`.
3. **Main drift safely handled**: The workflow already holds a trusted `w.state['base_revision']` from planning. If `main` moved while the workflow was stranded, Muse's candidate submission hits the existing delivery `prepare` -> `main_changed` -> `_request_rebase()` path automatically, avoiding any new intermediate phase.
4. **Full downstream gates preserved**: Because `approved_plan` matches the exact 3-key schema validated by `validate_target()` ([work_external_review_context.py](../../../deploy/mission-control/agent-runner/work_external_review_context.py#L57-L61)), independent native Codex `code_review`, protected-gate attestations, exact-source Jenkins `checks`, guarded `merge`, and `post_merge_verifications` execute identically to a reviewer-approved plan.

---

## 6. Narrowness & No-Tampering Guarantees

Why this contract cannot devolve into a general state-change backdoor:

1. **Single hardcoded transition**: Only `escalated -> implement` is expressible. The command has no `target_phase`, `target_status`, or arbitrary `state` patch fields.
2. **Single failure class**: Only plan-revision exhaustion (`target_kind = 'plan'`, `review.status = 'revise'`, planning `used = 20 / exhausted` or legacy plan-budget exhaustion, `code_revisions = 0`, no `approved_plan`/`candidate`/`merge_receipt`) is admitted. It explicitly **cannot**:
   - override a workflow in any non-`escalated` phase (`plan`, `plan_review`, `plan_revision`, `implement`, `code_review`, `checks`, `repair`, `merging`, `completed`, `cancelled`),
   - override a workflow escalated for grant revocation, reviewer `escalate` verdict, reviewer-correction malformed-output exhaustion, code-review/CI exhaustion, or merge failure,
   - override a user-cancelled workflow (checked via `coding_control_receipts`),
   - skip or weaken independent Codex `code_review`, protected-gate attestations, exact-source Jenkins `checks`, or guarded `merge` verification.
3. **One-shot DB enforcement**: `UNIQUE(workflow_id)` on `forge_external_workflow.plan_approve_overrides` enforces at most one plan approve-override per workflow at the PostgreSQL constraint layer; an `UPDATE`/`DELETE` immutability trigger (`P0009`) prevents erasure or alteration.
4. **Zero history or budget tampering**: Never alters `reviews`, `artifacts`, prior `assignments`, `coding_retry_events`, `coding_retry_event_admissions`, `workflows.plan_revisions`, `workflows.snapshot`, or `workflows.policy_digest`.
5. **No model/agent/runner access**: External Muse runners, native workers, delivery workers, and the automatic coordinator have zero SQL grants and zero wired ports to invoke the override.
6. **Exact-artifact binding**: Cannot approve an uninspected or mutated plan; every override cryptographically pins `plan_assignment_id`, `plan_sha256`, `source_review_id`, `expected_workflow_revision`, and `expected_root_revision`.

---

## 7. Verification Scope (When Implemented)
- **Isolated PostgreSQL & unit tests**: Verify exact happy path (`escalated` after 20 `revise` rounds -> `implement` queued -> Muse `claim` receives `approved_plan` -> full `prepare`/`code_review`/`checks`/`merge` completes), idempotent replay, `idempotency_conflict`, second-override denial (`UNIQUE(workflow_id)`), and rejection of every forbidden case (wrong phase, user-cancelled, reviewer `escalate`, reviewer-correction exhaustion, code-stage escalation, mismatched `plan_sha256`/revision, revoked scope grant, active child task/attempt/action).
- **Immutability & privilege checks**: Confirm `UPDATE`/`DELETE` on `plan_approve_overrides` raises `P0009`, prior `reviews` and `coding_retry_events` rows are byte-identical before and after override, and `muse`/`forge_coding_worker`/`forge_coding_reconciler` roles have zero write access; run updated `verify-external-workflows.sql` and `verify-muse-read-access.sql`.
