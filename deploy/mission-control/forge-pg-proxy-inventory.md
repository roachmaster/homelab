# forge-pg-proxy inventory (required artifact)

Generated from the `deploy/mission-control/agent-runner` sources at base
57a0aca plus the SQL migrations and `grant-runner.sql`. Every data access
below gets exactly one proxy operation (see `forge_pg_proxy.py`
`OPERATIONS`). No ad-hoc SQL from clients, ever.

## 1. Callers and their DB roles

| Caller (proxy identity) | DB role | Owner scope (`forge.coding_owner`) |
|---|---|---|
| `coordinator` (agent-runner / WorkStore / WorkFlow / portal) | `forge_agent` | `mission-control` |
| `worker` (forge-coding-worker) | `forge_coding_worker` | `mission-control` |
| `reconciler` (forge-coding-reconciler) | `forge_coding_reconciler` | `mission-control` |
| `gpu` (forge-gpu-worker) | `forge_gpu_worker` | `mission-control` |
| `muse` (Muse external transport) | `muse_proxy_writer` (mutations) / `muse_events` (reads) | `mission-control` |
| `operator` (operator CLIs, incl. plan-override) | `forge_agent` | `mission-control` |

The owner scope comes only from the proxy's trusted caller
configuration, never from a request field.

## 2. SECURITY DEFINER function operations (reuse, §5)

Every function below is one proxy operation, called as the caller's
DB role so `session_user` gates pass unchanged. Arg validation is by
the registry (ordered parameters, typed).

### forge_agent.* (caller: coordinator)
`amend_agent_retry_policy`, `coding_make_feedback`,
`coding_queue_approved_review`, `coding_schedule_repair`,
`coding_workflow_role_granted`, `delivery_project_head`,
`delivery_repair_admit`, `delivery_repair_bind`,
`owner_continuation_capability`, `retry_event_adopt_legacy`,
`retry_event_bind_review`, `retry_event_get`, `retry_event_observe`,
`retry_event_project`, `retry_event_reserve`, `retry_event_resolve`.
(These are the plain functions the coordinator calls directly; the
SECURITY DEFINER API functions above are the bulk of its work.)

### forge_external_workflow_api (caller: coordinator)
`enqueue_v2`, `service_scope`.

### forge_external_workflow_worker_api (caller: worker)
`read_context`.

### forge_coding_worker_api (caller: worker)
`advertise`, `claim`, `renew`, `checkpoint`, `report_progress`,
`read_plan`, `queue_publication`, `append_result`, `publish`,
`cancellation_requested`, `begin_usage_poll`, `next_refresh`,
`record_usage`, `fail_usage_poll`, `record_stop`, `read_feedback`,
`read_native_delivery_context`, `read_review_target`,
`read_workflow_feedback`, `project_revisions`, `next_refresh`
(internal helpers `_lock_task_and_attempt`, `_validate_provider_snapshot`,
`_validate_stop_proof`, `_validate_usage_json` are not operations).

### forge_coding_reconciler_api (caller: reconciler)
`delivery_available`, `delivery_checkpoint`, `delivery_claim`,
`delivery_finish`, `delivery_observe`, `delivery_project_head`,
`delivery_release_checkpoint`, `delivery_release_claim`,
`delivery_release_finish`, `delivery_release_renew`,
`delivery_release_start_allowed`, `delivery_renew`,
`delivery_start_allowed`, `expire_leases`, `merge_claim`,
`merge_finish`, `merge_intent`, `merge_renew`,
`native_branch_first_replay`, `native_branch_published`,
`native_ci_context`, `native_ci_status`, `native_merge_failure`,
`owner_continuation_capability`, `owner_continuation_fenced`,
`pending_reconciliations`, `post_merge_available`,
`post_merge_checkpoint`, `post_merge_claim`, `post_merge_finish`,
`post_merge_renew`, `post_merge_start_allowed`, `publication_claim`,
`publication_finish`, `publication_renew`, `reconcile`,
`record_hosted_checks`, `review_claim`, `review_finish`,
`review_renew`.

### forge_agent_worker_api (caller: gpu)
`advertise`, `next_model`, `claim`, `renew`, `publish`,
`lock_attempt`.

### forge_external_agent_api (callers: muse via writer; coordinator via service role)
Mutating (writer principal): `advertise`, `claim`, `finish`,
`progress`, `release`, `renew`. Read-only (`muse_events` direct):
`artifact`, `capabilities`. Service role (`forge_agent`):
`enqueue`, `catalogue`, `artifact_by_task`, `reconcile`,
`event_binding`.

## 3. Direct-write entry points (new, §5; schema `forge_pg_proxy_api`)

One narrow SECURITY DEFINER function per (table, verb), owned by
`forge_agent_owner`, EXECUTE granted only to the caller roles
(reachable only through the proxy). Parameters are the columns the
codebase writes; a column is set only when its argument is non-NULL
(`col = COALESCE(p_col, col)`), so one function covers every
statement shape for that table/verb. No dynamic SQL.

| Table | Insert | Update | Delete |
|---|---|---|---|
| `forge_agent.coding_tasks` | `coding_task_insert` | `coding_task_update` | — |
| `forge_agent.coding_workflows` | `coding_workflow_insert` | `coding_workflow_update` | — |
| `forge_agent.coding_workflow_reviews` | `coding_workflow_review_insert` | `coding_workflow_review_update` | — |
| `forge_agent.coding_workflow_actions` | `coding_workflow_action_insert` | `coding_workflow_action_update` | — |
| `forge_agent.coding_events` | `coding_event_insert` | — | — |
| `forge_agent.coding_control_receipts` | `coding_control_receipt_insert` | — | — |
| `forge_agent.coding_reviews` | `coding_review_insert`, `coding_review_insert_copy` (INSERT…SELECT from a source review; review_no is max+1 for the task) | `coding_review_update` | — |
| `forge_agent.coding_plan_decisions` | `coding_plan_decision_insert` | — | — |
| `forge_agent.coding_review_policy` | `coding_review_policy_upsert` | — | — |
| `forge_agent.coding_review_budget` | `coding_review_budget_insert` | — | — |
| `forge_agent.coding_worker_grants` | `coding_worker_grant_insert` | — | — |
| `forge_agent.coding_usage_refresh_requests` | `coding_usage_refresh_request_insert` | — | — |
| `forge_agent.conversations` | `ide_conversation_insert` | — | `ide_conversation_delete` |
| `forge_agent.runs` | `ide_run_insert` | `ide_run_update` | `ide_run_delete` |
| `forge_agent.messages` | `ide_message_insert` | — | — |
| `forge_agent.tool_calls` | `ide_tool_call_insert` | `ide_tool_call_update` | — |
| `forge_agent.tool_results` | `ide_tool_result_insert` | — | — |
| `forge_agent.run_events` | `ide_run_event_insert` | — | — |
| `forge_agent.ide_workspace_tabs` | `ide_workspace_tab_insert` | `ide_workspace_tab_update` | `ide_workspace_tab_delete` |
| `forge_agent.ide_workspace_history` | `ide_workspace_history_insert` | `ide_workspace_history_update` | — |
| `forge_agent.ide_chats` | `ide_chat_insert` | `ide_chat_update` | — |
| `forge_agent.mail_threads` | `mail_thread_insert` | `mail_thread_update` | — |
| `forge_agent.mail_messages` | `mail_message_insert` (sequence computed max+1 per thread) | — | — |
| `forge_agent.jenkins_events` | `jenkins_event_insert` | — | — |
| `forge_agent.jenkins_event_deliveries` | `jenkins_event_delivery_insert` | `jenkins_event_delivery_update` | — |
| `forge_agent.jenkins_observations` | `jenkins_observation_insert` | — | — |
| `forge_agent.inference_results` | — | — | `inference_result_delete` |
| `forge_external_workflow.workflows` | `ext_workflow_insert` | `ext_workflow_update` | — |
| `forge_external_workflow.tasks` | `ext_task_insert` | — | — |
| `forge_external_workflow.contexts` | `ext_context_insert` | — | — |
| `forge_external_workflow.reviews` | `ext_review_insert` | `ext_review_update` | — |
| `forge_external_workflow.actions` | `ext_action_insert` | `ext_action_update` | — |
| `forge_external_workflow.plan_approve_overrides` | `ext_plan_approve_override_insert` | — | — |
| composite | — | `ext_workflow_cancel` (cancellation: children, actions, workflow, task revision, control receipt, events, in one transaction) | — |
| composite | — | `coding_workflow_cancel` (native cancel: workflow branch with task transitions + events + receipt, or single-task branch with the revision-gated cancel/cancel-request transition) | — |
| composite | — | `workflow_action_lease` (next pending action FOR UPDATE SKIP LOCKED + lease, atomically) | — |

Conventions: where the codebase actually
clears a nullable column, the entry point takes an explicit
`clear_<column>` boolean (COALESCE-only updates cannot clear a
column); the registry lists exactly the clear flags the codebase
uses. Computed values stay inside the function: sequences and
positions are max+1, review numbers are max+1, lease expiries and
retry delays are computed from `*_secs` parameters, and the two
cancel transitions are boolean flags. Bulk deletes take an array of
ids and return `{"deleted": [...]}` with the guard the codebase
uses (no runs exist for a conversation; owner-scoped tab ids).

## 4. Read operations

Reads run as the caller's DB role (SELECT grants are preserved).
Two kinds, both fixed SQL in the registry — no client-supplied SQL:

- **Point/list reads** (`read.<table>`): one operation per table,
  equality filters only on that table's registered key columns
  (PK and the indexed lookup columns the codebase uses), fixed
  column list (all columns), fixed `ORDER BY` + `LIMIT`.
  Tables: every table in §3 plus `coding_attempts`,
  `coding_result_chunks`, `coding_reconciliation_receipts`,
  `coding_publications`, `coding_worker_readiness`,
  `gpu_worker_readiness`, `coding_retry_events` (+ children),
  `coding_feedback`, `coding_review_budget`, `coding_hosted_checks`,
  `coding_provider_usage`, `coding_retry_event_admissions`,
  `coding_retry_event_observations`, `plan_approve_overrides`,
  `mail_threads`, `mail_messages`, `forge_external_agent.*`
  (assignments, artifacts, readiness, scope_grants).
- **Aggregate reads** (`read.<table>.count` / `.exists`): fixed
  `count(*)` / `EXISTS` variants for the status checks the codebase
  performs (e.g. pending-action counts, readiness freshness).

## 5. Notification consumers (LISTEN channels)

| Channel | Consumers | Proxy transport |
|---|---|---|
| `forge_agent_jobs` | GPU worker, queue store | proxy LISTEN + long-poll |
| `forge_agent_events` | queue store | proxy LISTEN + long-poll |
| `forge_agent_results` | queue store | proxy LISTEN + long-poll |
| `forge_coding_jobs` | native worker (WorkStore) | proxy LISTEN + long-poll |
| `forge_jenkins_events` | jenkins store | proxy LISTEN + long-poll |
| `forge_external_agent_jobs` | external-agent enqueue | proxy LISTEN + long-poll |
| usage-refresh channel (`USAGE_REFRESH_NOTIFY_CHANNEL`) | worker store | proxy LISTEN + long-poll |

The proxy holds one LISTEN connection per channel and fans out
notifications to subscribed callers. `pg_notify` calls are issued
by the proxy itself after the triggering write commits (the
`pg_notify` statements in `queue_store.py` and `jenkins_store.py`
become proxy operations).

## 6. Connection paths (HBA / roles / auth)

Pre-cutover (current): `host forge_agent forge_gpu_worker
127.0.0.1/32 scram-sha-256`, `host forge_agent muse_events
127.0.0.1/32 scram-sha-256`, `host all all 127.0.0.1/32 reject`,
`host replication all 127.0.0.1/32 reject` (plus the distribution
defaults for local peer + other hosts). `forge_agent`,
`forge_coding_worker`, `forge_coding_reconciler` connect over the
local Unix socket (peer) and/or TCP from the runner host.

Post-cutover: write-capable roles (`forge_agent`,
`forge_coding_worker`, `forge_coding_reconciler`,
`forge_gpu_worker`, `muse_proxy_writer`) are admitted ONLY via
local peer authentication from the proxy OS user
(`forge-pg-proxy`); no TCP/SCRAM admission for them. `muse_events`
keeps its SCRAM host path (read-only; enforced by grants). All
other paths are rejected. Old SCRAM credentials for the
write-capable roles are rotated at cutover. Non-proxy sessions are
terminated at cutover.

## 7. What the proxy does NOT cover

- Superuser / human DBA access (by policy, logged by procedure).
- The Rust portal host (it reaches Postgres only through the
  agent-runner API, never directly).
- `work_pg_fixture.py` (test-only fixture; not a runtime caller).
