# Coding assignment admission

The first coding-agent implementation stores assignments for later execution.
It is **disabled by default and has not been enabled on the live portal**.
There is no coding worker, agent invocation, result reader, acceptance action,
account polling, or Agents application screen in this slice. Admitted work stays
`queued` until explicitly cancelled.

The [dispatch proposal](proposals/forge-coding-agent-dispatch.md) describes the
remaining execution and review flow. The [usage proposal](proposals/forge-agent-usage.md)
remains proposed. Its provider allowances must never be inferred from task counts.

## Ownership and normal entry points

| Owner | Selected collaborator | Lifetime and effects |
| --- | --- | --- |
| Rust host bootstrap | Optional `WorkConfig` → `WorkRelayFactory` → `Singleton<WorkRelay>` | Registration validates without opening files. Creation after registry freeze reads the selected bearer credential. Work grants are independent of node inspection and Chat/Mail/GPU flags. |
| Runner bootstrap | Optional frozen `WorkConfig` → `WorkServiceFactory(config, connect_factory)` | Missing configuration returns `None` without file, database, or CLI effects. Configuration loads only from an explicit absolute startup path. Factory creation injects a ready service/catalog/store and opens no connection. |
| Runner `Server` | Injected `WorkService` | Existing authenticated `POST /v1/dispatch`; no new listener. Configured project/profile/mode grants govern admission, including retries. A replacement catalog cannot expand them. |
| `WorkStore` | Injected connection callable, owner and allowed projects | A short connection/transaction per operation, always closed. All task, event, receipt and cursor queries scope the trusted owner/project. Workers never look up a registry; no worker exists yet. |

Source: [Rust host](../../src/rust/forge-ide-web-host/src/work.rs),
[runner API/service](../../deploy/mission-control/agent-runner/work_api.py),
[configuration/catalog](../../deploy/mission-control/agent-runner/work_config.py),
[DAO](../../deploy/mission-control/agent-runner/work_store.py).

## Implemented version 1 contract

Public requests use `POST /forge/v1/dispatch`; the host relays to the existing
private runner `POST /v1/dispatch`. Require the existing four Forge headers:
destination, envelope version `1`, contract version `1`, and request ID. The
browser boundary additionally checks its session, Origin and CSRF token. The
private runner requires its bearer credential. Neither accepts a request owner.

Every body requires `kind`, matching `request_id`, and `project_alias`.
Unknown fields and duplicate headers/JSON fields are rejected. Responses use
`{"protocol_version":1,"event":{"kind":"...","request_id":"..."}}`.

| Destination / request kind | Additional request fields | Success |
| --- | --- | --- |
| `ide.work.submit` / `work_submit` | `base_revision`, `mode`, `agent_profile`, `prompt`, `acceptance_criteria`, `idempotency_key` | `202 work_submitted`: task/project, revision `1`, status `queued`, immutable recipe digest. |
| `ide.work.get` / `work_get` | `task_id` | `200 work_opened`: scoped task summary plus prompt and acceptance criteria. |
| `ide.work.list` / `work_list` | Optional `status_filter`, `limit`, `cursor` | `200 work_listed`: project, `tasks`, nullable `next_cursor`. |
| `ide.work.events` / `work_events` | `task_id`; optional `after_sequence`, `limit` | `200 work_events`: task/project, status/revision, ordered events and next sequence. |
| `ide.work.cancel` / `work_cancel` | `task_id`, `expected_revision`, `idempotency_key` | `200 work_cancel_recorded`: task/project, numeric base/result revision, status `cancelled`. Only queued work can transition. |

Unknown/deferred `ide.work.*` destinations return `404 work_failed/not_found`
after authentication. Supported routes with Work disabled return `503 unavailable`.
Invalid requests use `400`; denied grants `403`; cross-owner/project task reads
`404`; idempotency/revision/non-cancellable conflicts `409`; full queue or unavailable
storage `503`. The host validates upstream reply shape, status, correlation,
scope, digest, timestamps and pagination before returning it.

Prompt text is nonblank and at most 8,192 UTF-8 bytes; acceptance criteria are
nonblank and at most 2,048. The raw request ceiling is 16,384 bytes, including JSON
escaping/framing. Identifiers use ASCII letters, digits, `_`, `.` and `-` (64
characters; idempotency keys 128). Task/cursor IDs are canonical lowercase UUIDs;
base revisions are 40 lowercase hexadecimal characters. Modes are `plan`,
`review`, or `implement` as **intent only**, with independent grants.

List pages default to 20, maximum 50; event pages default to 50, maximum 100.
Lists order ascending by `(created_at, task_id)` and resolve cursor anchors inside
the same owner/project before filtering/limiting. An unavailable/foreign cursor
is invalid. Event sequences are per task; empty pages preserve `after_sequence`.
Revisions/sequences stay within JavaScript's exact integer range.

## Transactions, retries and token policy

Admission serializes capacity/idempotency decisions per owner/project with a
transaction advisory lock. Exact user intent is checked **before** queue capacity
and recipe resolution. A new task pins the nonsecret recipe, inserts event 1 and
calls `pg_notify('forge_coding_jobs', '')` in the same transaction; only after
commit does the caller receive `202`. Notifications contain no task/prompt data
and have no active coding consumer in this slice.

The recipe records version, backend, model, resource profile, mode and
`generation_token_limit: null`; a canonical SHA-256 digest binds those values.
There is no inherited llama.cpp/Chat/Mail generation-token cap. Provider limits
still apply when a future worker executes. Request-byte and queue bounds are
separate controls and do not estimate model allowance.

Exact submit retries return the original business receipt, even after cancellation
or catalog drift, with the current request ID. Changed intent conflicts. Revoked
grants still reject a retry. Queued cancellation locks the task, checks immutable
control receipts before revision/state, then atomically writes revision 2, event 2
and its receipt. Same-key retries replay; a new cancel key on cancelled work conflicts.
Event pages use one repeatable-read snapshot for consistent status and event rows.

Migration [0009](../../deploy/mission-control/sql/migrations/0009_coding_admission.sql)
adds only `coding_tasks`, `coding_events`, and `coding_control_receipts`.
Runner grants are SELECT/INSERT/UPDATE on tasks and SELECT/INSERT on events and
receipts; no DELETE, worker role or worker routines are added. Existing Mail/GPU
grants remain separate. Production migrations have not been applied by this task.

## Configuration and remaining rollout

The native host's optional `work` object specifies a loopback `endpoint`, absolute
`token_file`, and independent `allowed_projects`, `allowed_profiles`, `allowed_modes`.
The runner's optional `FORGE_RUNNER_WORK_CONFIG` points to trusted JSON containing
`owner`, `allowed_projects`, `allowed_modes`, `profiles`, and optional `max_pending`
(default 32, range 1–256 per owner/project). Each profile names only its backend,
model and resource profile; arbitrary paths, commands and environment entries are rejected.

See the [host guide](../../src/rust/forge-ide-web-host/README.md) and
[runner guide](../../deploy/mission-control/agent-runner/README.md) for configuration
examples and disposable test commands. Keep both settings absent in production
until the later worker and supervision gates are implemented. The next slice is
fenced attempts and full-result storage with fake execution tests; real CLI scopes,
reconciliation/resume/acceptance, usage collection and generated UI follow separately.
