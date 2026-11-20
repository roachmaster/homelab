# Coding-agent workflow

**Source checkpoint: 19 November 2026.** The repository now implements durable
coding work, native CLI execution, result paging, explicit review/resume, provider
usage collection and the generated Agents view. Work and Usage remain independent,
optional capabilities. The ABI7 portal, API runner and coding worker were deployed
and verified on 19 November 2026 from code commit `679088c`. Open
[Agents](https://portal.thedevex.com/agents) or Home → Agents. The
[deployment checkpoint](../design/implementation-status.md#observed-deployment-checkpoint)
records the scope and checks. The earlier admission-only slice is superseded.

The [dispatch design](proposals/forge-coding-agent-dispatch.md) and
[usage design](proposals/forge-agent-usage.md) retain the sequence diagrams and
explain how the implementation maps to the original design.

## Portal and ownership

Home links to `/#agents`; `/agents` redirects to that declared same-tab view.
Existing global navigation stays compact. Rust/WASM owns selection, drafts,
request guards, actions and polling; generated main JavaScript renders and passes
actions, and generated I/O worker JavaScript performs admitted requests. The
browser ABI is **7**, while the HTTP contract stays version **1**. Deploy the
HTML/CSS/JavaScript/WASM bundle atomically.

The catalog supplies permitted projects, their worker-observed Git revisions,
configured provider/model profiles and available modes. Users select catalog
entries rather than typing repository paths, model commands or commit hashes.
A fresh worker advertisement establishes readiness; a launcher does not.

| Composition owner | Injected collaborator | Authority and lifetime |
| --- | --- | --- |
| Rust host bootstrap | Optional Work and Usage configurations → factories → host-owned relays | Validate/freeze before creation. Session, Origin, CSRF, strict envelope and independent project/profile/mode grants precede private I/O. |
| Private runner bootstrap | `WorkServiceFactory` and `UsageServiceFactory` with injected stores | Each capability is explicitly enabled. An absent configuration returns `None` with no file, database or CLI effects. The existing authenticated `POST /v1/dispatch` remains the single private entry point. |
| Runner stores | `WorkStore` and `UsageStore` with connection factories | Short owner/project/profile-scoped transactions; no provider credentials, direct CLI execution or caller-supplied owner. |
| Native worker bootstrap | `CodingWorkerFactory`, `WorkerDB`, executor/workspace factories and optional `UsageCollector` | Receives ready ports, never a global registry. Scoped database credentials stay in the parent; no inherited DB, bearer or API keys enter CLI environment. |
| Trusted reconciler | Separate reconciler connection and narrow functions | Verifies the old scope stopped and the locked workspace digest before issuing a resume receipt. It does not automatically restart work. |

Pure DTOs, policy values and reducers do not need empty registrars. Configuration
and factory selection determine effects; worker code never retrieves dependencies
from a global registry.

## Single gateway and wire contract

Public `POST /forge/v1/dispatch` relays through the admitted Rust host to private
`POST /v1/dispatch`. Require `Forge-Destination`, `Forge-Envelope-Version: 1`,
`Forge-Contract-Version: 1`, and `Forge-Request-Id` matching body `request_id`.
Unknown or duplicate fields and mismatched kinds are rejected. Responses use
`{"protocol_version":1,"event":{"kind":"...","request_id":"..."}}`.

All Work bodies except `work_catalog` require `project_alias`. Task IDs are
canonical UUIDs, base revisions are 40 lowercase hex characters, and digests are
64 lowercase hex characters. Revisions/sequences remain JavaScript-safe integers;
event sequence is independent of task revision.

| Destination | Success | Implemented behavior |
| --- | --- | --- |
| `ide.work.catalog` | `200 work_catalog` | Grants-filtered projects, profiles, model/modes and fresh worker readiness; no host paths or auth files. |
| `ide.work.submit` | `202 work_submitted` | Immutable intent/recipe receipt after task, first event and notification commit. |
| `ide.work.get` | `200 work_opened` | `event.task` contains scoped task details, nullable attempt, published result and current reconciliation receipt. |
| `ide.work.list` | `200 work_listed` | Bounded task summaries and scoped keyset cursor; no full prompt in lists. |
| `ide.work.events` | `200 work_events` | Ordered events strictly after the supplied sequence, plus current status/revision. |
| `ide.work.cancel` | `200 work_cancel_recorded` | Queued work cancels immediately; running work becomes `cancel_requested` until stop is established. |
| `ide.work.result` | `200 work_result_page` | Manifest-bound UTF-8 page of a published immutable result. |
| `ide.work.accept` | `200 work_accepted` | Explicitly accepts stopped `awaiting_review` work as `completed`; never merges code automatically. |
| `ide.work.resume` | `202 work_resumed` | Explicit user action requiring revision, selected admitted profile and a current trusted reconciliation receipt. |

Prompt limit is 8,192 UTF-8 bytes; acceptance criteria 2,048; raw request ceiling
16,384 including JSON escaping. Lists default to 20/max 50; event pages default
to 50/max 100. Disabled supported routes return sanitized `503 unavailable`;
unsupported destinations return `404`. Cross-owner/project reads are `404`,
denied grants `403`, invalid input `400`, and replay/revision/review conflicts
`409`. The host validates upstream reply shape, scope and correlation.

## Durable execution and explicit recovery

Admission serializes capacity/idempotency decisions per owner/project. Exact
intent replay is checked before capacity and recipe resolution; current grants
still apply. A task, sequence-1 event and empty
`pg_notify('forge_coding_jobs', '')` commit together. Notifications carry no
prompt and only wake work discovery; committed rows are authoritative.

Execution-capable profiles pin recipe **version 2**, including backend, explicit
model, mode, resource profile, `generation_token_limit: null` and immutable
`policy_digest`. The worker validates the complete canonical recipe against
`forge-coding-v3` before workspace creation or CLI spawn. This explicitly versioned
policy approves Gemini commands, scoped implementation writes and read-only
documentation URLs without headless approval prompts. Plan/review remains
read-only; OS isolation and database/canonical-source masks remain unchanged.
Migration13 permits policy1, policy2 and policy3 history while exact advertised policy
digests keep claims separate. Previous attempts retain their original policy.
Admission-era version-1
recipes remain readable and are never silently upgraded or executed.

The worker claims with `FOR UPDATE SKIP LOCKED`, uses a 60-second database-time
lease and renews every 10 seconds through setup, execution and cleanup. A durable
workspace reservation and OS file lock allow one writer. The first attempt uses
an isolated Git worktree at the approved base. Plan/review mounts it read-only;
implement can write only the approved isolated tree and its Git metadata.

Only the configured real CLI binary is invoked with fixed argument structure,
`shell=False` and a sanitized environment. Unrelated submissions start fresh;
Antigravity may reuse an explicitly resumed task's parent-verified conversation
through `--conversation UUID`. Never use the global latest `--continue`, because
all child scopes share the same mounted workspace pathname. Codex stays ephemeral.
Native provider/model
limits apply. No Chat/Mail/llama.cpp completion cap is inherited, and no purchased
credit fallback, automatic account rotation or implicit conversation continuation
is enabled. A successful exit alone is insufficient: the parser requires a
coherent owned final result, nonempty text, no unresolved permission/error state
and confirmed process-scope stop.

The selected worker factory injects an Antigravity session provider. A private
bounded hint binds task/project/workspace/base revision/profile/model/recipe,
local account and binary identity. It is reusable only with the existing trusted
workspace reconciliation digest and the receipt's immediately preceding attempt,
is invalidated before execution, and is saved
again only after a coherent stopped result. It cannot grant a lease, replay an
assignment or choose another task's CLI history. The child cannot access the
parent's session records.

AGY output remains `stream-json`; one stdin JSON user event and EOF bound each
attempt. Only terminal usage is normalized. `cache_read_tokens` / `cached_tokens`
are aliases, not additive metrics, and cache-only observations are partial.
Cross-process resumed counters have no verified zero baseline, so they remain
unknown with `unverified_baseline` provenance instead of double-counting earlier
usage. Unsupported headless allowance stays **Not reported**, with no OAuth
polling or generation-based quota probe. Context reuse is an opportunity for
native cache hits, not a measured quota saving.

The local launcher has separate bounded SIGTERM → SIGKILL/reap cleanup on normal
and exceptional paths. A successful task requires both this cleanup and the
independent systemd cgroup stop proof. The source policy3 requires migration13
and coordinated admission/worker configuration; dated policy2 deployment records
do not establish that it is already enabled.

A root-managed supervisor launches a fixed namespace/privilege-drop wrapper.
The outer launcher needs namespace authority; it is not the agent process.
`unshare` establishes mount/PID isolation, then `setpriv` changes to the approved
signed-in UID/GID, clears groups and all capability sets, and sets
`no_new_privs` before executing the CLI. The CLI therefore runs unprivileged.
The transient unit uses a strict filesystem, control-group cleanup and a wall
deadline. Canonical repositories, database sockets/credentials, parent control
state and supervision sockets are hidden. Plan/review cannot write the worktree.
Implement remains separately opted in after actual isolation checks.

Lease expiry, a crash or uncertain stop enters `needs_reconcile`, retains the
workspace reservation and never automatically requeues. The separate trusted
reconciler verifies the supervision unit and descendants stopped, locks and
hashes the actual workspace, and issues a receipt bound to current task revision.
`ide.work.resume` requires that receipt; the next worker checks the same digest
under its lock before spawning. Quota/auth/permission blocks do not automatically
retry or swap agents. The user must review and explicitly resume with an admitted
profile. No browser boolean can assert process stop or workspace safety.

## Public plans and live progress

Gemini tasks publish an explicit public plan before work, then summaries of
observable milestones and blockers. A self-contained `report_progress.py` helper
writes immutable sequential JSON reports inside the already admitted attempt
scratch area. It receives no database credentials, attempt IDs or writer tokens.
The parent owns a bounded directory reader and injects a scoped progress callback
into the selected command runner. Pure validation stays outside provisioning.

```mermaid
sequenceDiagram
    participant A as Gemini CLI
    participant H as Scratch helper
    participant W as Supervising worker
    participant D as PostgreSQL
    participant P as Portal Rust model
    A->>H: plan(public steps), then progress(public milestone)
    H->>H: Atomic numbered JSON report
    W->>H: Read and validate bounded consecutive reports
    W->>D: report_progress(attempt token, ordinal, stage, summary)
    D->>D: Fence owner/lease; commit checkpoint; notify
    D-->>W: Idempotent receipt
    P->>D: Existing admitted work_events route through host/runner
    D-->>P: Whitelisted public progress only
    P->>P: Render plan and recent current-attempt milestones
    A-->>W: Final result
    W->>W: Confirm process scope stopped
    W->>D: Publish immutable result: awaiting_review
```

Migration12 adds a narrow worker-only `report_progress` function and report
identity constraints on existing checkpoint events. Report ordinal1 is `plan`;
2–128 are `progress`. Each summary is nonblank, at most4096 UTF-8 bytes, and
rejects control characters except newline/tab. Exact receipt replay is harmless,
even after the lease expires; new reports require the current attempt's active
owner/token/lease/deadline fences. Conflicting or skipped ordinals fail. Neither
reporting nor notifications changes task status, task revision or stop authority.
A transient lost acknowledgement retries the same report, never model execution.

The existing event reader optionally returns a typed `progress` object containing
only `attempt_id`, report `sequence`, `stage` and `summary`; it never forwards opaque
payloads, provider transcripts, tool arguments or private reasoning. The global
event sequence remains independent of the report ordinal and task revision. Rust
loads history for finished tasks, retains the plan plus the latest20 milestones,
filters other attempts and refreshes when new events arrive at unchanged revision.
The complete bounded history remains in PostgreSQL. Receipt dates, rather than
worker heartbeat times, identify agent updates.

An otherwise successful Gemini run requires a persisted public plan. This is a
reporting requirement, not an OS write gate or proof that the plan was followed.
Final publication still requires the coherent result and verified stopped scope;
acceptance remains explicit. Plan/review repository mounts stay read-only while
the allowed command helper writes scratch. Codex keeps its existing sandbox and
terminal reporting; its read-only inner sandbox has no admitted scratch-write
channel in this slice. Existing attempts retain their original instructions and
cannot acquire this reporting path while already running.

## Full results and uncertain acknowledgements

Result text is persisted in UTF-8-safe chunks (up to 16 KiB each, 1,024 chunks,
16 MiB total). Publication verifies contiguous chunks, byte count and SHA-256
manifest and makes them visible atomically. Staging chunks are unreadable. A
storage/framing bound produces an explicit failure rather than silently cutting
an otherwise successful reply.

`ide.work.result` uses a manifest-bound `<sha256>.<byte_offset>` cursor and page
sizes 4–16,384 bytes. Browser state holds one page and bounded cursor history,
not an unbounded concatenation. Selection/request/attempt/manifest guards reject
stale replies. Preview/page size never changes the persisted result.

Exact immutable publish/control receipt replay precedes state/CAS rejection;
a lost acknowledgement can be repaired without rerunning the model or changing
an accepted result. Parent-owned result spooling and reconciliation preserve
uncertain outcomes separately from normal retries. Review acceptance records
user approval; Git merge/push remains an independent action.

## Database fences

Migration [0009](../../deploy/mission-control/sql/migrations/0009_coding_admission.sql)
remains unchanged. [0010](../../deploy/mission-control/sql/migrations/0010_coding_execution_and_usage.sql)
extends tasks/events/control receipts and adds worker grants/readiness, attempts,
result chunks, reconciliation receipts, provider snapshots, private identity
bindings and metadata-refresh requests.

- `forge_agent` receives only the runner's required task/event/receipt/usage
  privileges, with no worker grant or private identity-table access.
- `forge_coding_worker` has no direct table/column access; it can only execute
  scoped `forge_coding_worker_api` functions. Administrator-owned grants bind
  `session_user` to owner/project/profile/mode.
- `forge_coding_reconciler` uses a separate connection and only the trusted
  expiry/reconciliation routines. Worker credentials cannot mint receipts.
- `forge_gpu_worker` has no coding/usage access, including inherited, column or
  PUBLIC privileges. Existing GPU/Mail ownership stays separate.

Security-definer routines fix `search_path = pg_catalog, pg_temp`, qualify
objects and revoke PUBLIC execution inside the migration transaction. Worker
claim/renew/checkpoint/chunk/publish calls verify attempt token, incarnation,
lease/deadline and appropriate state. Public resume never fabricates a worker
or reconciler proof.

## Usage, configuration and rollout

The [usage implementation](proposals/forge-agent-usage.md) separates provider
allowance/account activity from Forge project job totals. Codex supports bounded
metadata-only account reads; Antigravity headless allowance is explicitly
`unsupported_headless`, while real per-job counters can still be recorded.
Refreshing metadata never submits a coding task or model turn.

Native Work and Usage grants and runner `FORGE_RUNNER_WORK_CONFIG` /
`FORGE_RUNNER_USAGE_CONFIG` are independently configured. Worker configuration
pins approved projects, signed account, binaries, models, policy, separate DB
roles and optional usage credentials. Nothing auto-enables from the presence of
a UI link or an installed CLI.

See the [runner guide](../../deploy/mission-control/agent-runner/README.md),
[host guide](../../src/rust/forge-ide-web-host/README.md),
[worker](../../deploy/mission-control/agent-runner/work_worker.py),
[executor](../../deploy/mission-control/agent-runner/work_executor.py) and
[current implementation reference](../design/implementation-status.md).
Source tests, disposable PostgreSQL checks, real process isolation, authenticated
metadata probes, end-to-end CLI result checks and a production activation record
are distinct evidence. The documentation update does not substitute for those
release checks or assert a live deployment.
